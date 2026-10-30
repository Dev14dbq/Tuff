//! What a player writes (chat, signs, books) on its way through the external
//! text filter, and what comes of it. Without a configured filter every text
//! passes at once and nothing is queued.

use mc_nbt::Tag;
use mc_protocol::filter::FilteredText;
use mc_protocol::{Position, text};
use tokio::sync::mpsc;

use super::windows::Windows;
use super::{PacketWriter, change_hands};
use crate::context::{Event, ServerContext};
use crate::interact::Actor;
use crate::textfilter::PlayerFilter;

/// A text the filter has judged, waiting to take effect.
#[derive(Debug)]
pub(super) enum Judged {
    Chat(FilteredText),
    Sign {
        position: Position,
        front: bool,
        lines: Vec<FilteredText>,
    },
    Book {
        slot: i32,
        title: Option<FilteredText>,
        pages: Vec<FilteredText>,
    },
}

/// A player's way to the filter.
pub(super) struct Lane {
    filter: Option<PlayerFilter>,
    tx: mpsc::UnboundedSender<Judged>,
}

impl Lane {
    /// The lane and the channel the judged texts come back on.
    pub fn new(
        ctx: &ServerContext,
        id: u128,
        name: &str,
    ) -> (Self, mpsc::UnboundedReceiver<Judged>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let filter = ctx.config.text_filter.as_ref().map(|f| f.context(id, name));
        (Self { filter, tx }, rx)
    }

    pub fn join(&self) {
        if let Some(f) = &self.filter {
            f.join();
        }
    }

    pub fn leave(&self) {
        if let Some(f) = &self.filter {
            f.leave();
        }
    }

    /// Sends `texts` to the filter. Returns `Some(texts)` untouched when
    /// there is no filter: the caller goes on at once. Otherwise `None`, and
    /// `judged` arrives on the channel once the service has answered.
    fn submit(
        &self,
        texts: Vec<String>,
        judged: impl FnOnce(Vec<FilteredText>) -> Option<Judged> + Send + 'static,
    ) -> Option<Vec<FilteredText>> {
        let Some(filter) = &self.filter else {
            return Some(
                texts
                    .iter()
                    .map(|t| FilteredText::pass_through(t))
                    .collect(),
            );
        };
        let tx = self.tx.clone();
        filter.texts(texts, move |verdicts| {
            if let Some(j) = judged(verdicts) {
                let _ = tx.send(j);
            }
        });
        None
    }

    pub fn chat(&self, message: String) -> Option<Judged> {
        let done = self.submit(vec![message], |mut v| v.pop().map(Judged::Chat))?;
        done.into_iter().next().map(Judged::Chat)
    }

    pub fn sign(&self, position: Position, front: bool, lines: [String; 4]) -> Option<Judged> {
        let lines = lines
            .iter()
            .map(|l| crate::interact::strip_sign_formatting(l))
            .collect();
        let done = self.submit(lines, move |lines| {
            Some(Judged::Sign {
                position,
                front,
                lines,
            })
        })?;
        Some(Judged::Sign {
            position,
            front,
            lines: done,
        })
    }

    /// `title` is trimmed first, as it is stored.
    pub fn book(&self, slot: i32, pages: Vec<String>, title: Option<String>) -> Option<Judged> {
        let split = move |mut all: Vec<FilteredText>, titled: bool| {
            let title = titled.then(|| all.remove(0));
            Judged::Book {
                slot,
                title,
                pages: all,
            }
        };
        let titled = title.is_some();
        let texts: Vec<String> = title
            .into_iter()
            .map(|t| t.trim().to_owned())
            .chain(pages)
            .collect();
        let done = self.submit(texts, move |all| {
            (!all.is_empty() || !titled).then(|| split(all, titled))
        })?;
        Some(split(done, titled))
    }
}

/// The player a judged text belongs to.
pub(super) struct Writer<'a> {
    pub ctx: &'a ServerContext,
    pub actor: Actor,
    pub name: &'a str,
    /// Their client's text filtering option.
    pub filtering: bool,
}

/// Lets a judged text take effect: chat goes out to the others, a sign takes
/// its lines, a book its pages.
pub(super) fn apply(
    who: &Writer,
    writer: &mut PacketWriter,
    windows: &mut Windows,
    judged: Judged,
) -> anyhow::Result<()> {
    match judged {
        Judged::Chat(text) => chat(who, &text),
        Judged::Sign {
            position,
            front,
            lines,
        } => {
            if let Some(event) = crate::interact::sign_update(
                who.ctx,
                who.actor,
                position,
                front,
                &lines,
                who.filtering,
            ) {
                let _ = who.ctx.events.send(event);
            }
        }
        Judged::Book { slot, title, pages } => {
            let index = match slot {
                0..=8 => 36 + slot as usize,
                40 => 45,
                _ => return Ok(()),
            };
            let mut changed = None;
            change_hands(who.ctx, who.actor.uuid, who.actor.entity_id, |hands| {
                let Some(Some(stack)) = hands.slots.get_mut(index) else {
                    return;
                };
                if crate::books::edit(
                    who.ctx,
                    stack,
                    who.name,
                    &pages,
                    title.as_ref(),
                    who.filtering,
                ) {
                    changed = Some(stack.clone());
                }
            });
            if let Some(stack) = changed {
                windows.inventory_slot(writer, index as i16, Some(stack))?;
            }
        }
    }
    Ok(())
}

fn line(name: &str, message: Tag) -> Tag {
    text::translate("chat.type.text", vec![text::plain(name), message])
}

/// Says the judged `message` in chat. Players who see filtered text get it
/// with the removed characters hidden; a message hidden entirely tells the
/// sender so.
fn chat(who: &Writer, message: &FilteredText) {
    let full = line(who.name, text::plain(&message.raw));
    if message.mask.is_empty() {
        let _ = who.ctx.events.send(Event::Chat(full));
        return;
    }
    let filtered = message
        .mask
        .apply_with_formatting(&message.raw)
        .map(|m| line(who.name, m));
    let _ = who.ctx.events.send(Event::FilteredChat {
        sender: who.actor.uuid,
        sender_filtering: who.filtering,
        full,
        filtered,
    });
    if message.mask.is_fully_filtered() && someone_sees_filtered(who) {
        let _ = who.ctx.events.send(Event::Message {
            uuid: who.actor.uuid,
            line: text::translate("chat.filtered_full", vec![]),
            overlay: false,
        });
    }
}

/// Whether some other player gets the filtered version of what the sender says.
fn someone_sees_filtered(who: &Writer) -> bool {
    let players = who.ctx.players.lock().expect("players lock");
    players
        .values()
        .filter(|p| p.profile.uuid != who.actor.uuid)
        .any(|p| who.filtering || p.text_filtering)
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.leave();
    }
}
