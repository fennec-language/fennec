//! Messages sent as their words: a flat message's fields as the compiler laid them out, handed over
//! through this thread's buffer, with the kind's number saying which words name blocks. The first
//! step of making messages cheap (the runtime lane's `cheap-messages-plan-2026-11-15-runtime.md`):
//! these calls only copy, and nothing calls them yet.
//!
//! - **Flat** is what [`Element::is_flat_message`] says, the one place it's decided: numbers,
//!   bools, strings, bytes, optional strings, and structs and enums of those. Each block such a
//!   message names holds only bytes, so a copy is the block's words with a count of its own. An
//!   enum's first word is its case's number, and only that case's fields name blocks.
//! - **The buffer** is the one a dict's value held in place crosses by (`fennec_dict_value_words`),
//!   as long as the run's widest kind. A send takes the words out of it before anything else, as
//!   another task on this worker may fill it once this one makes way, as a replay's turn does. A
//!   receive fills it last, just before it gives back.
//! - **A copy counts** as today's copy for a task does, block by block, so `FENNEC_COUNTS` reads
//!   the same whichever way a message goes, and a block two words name is copied once, with two
//!   owners.

use fennec_syntax::{Diagnostic, Span};

use super::super::heap::{self, IMMORTAL};
use super::super::{State, attempt, with};
use super::{CLOSED, DONE, Message, Turn, find, next, replayed, send_on};
use crate::packed::Element;

/// What `fennec_channel_receive_words` gives back when it ends with no message, as for a stop, and
/// the channel isn't closed or out of time. Its words then aren't the receiver's, whatever the
/// buffer holds.
pub(super) const NO_MESSAGE: i64 = 3;

/// A flat message on its way: its words, and copies of the blocks they name, which no heap holds
/// until the receiver takes them in.
pub(in crate::embed) struct Flat {
    /// Its kind's number in the program's table of kinds.
    kind: usize,
    words: Box<[u64]>,
    blocks: Vec<Box<[u64]>>,
    /// For each word that names a copy, where it is in `words` and which of `blocks` it names.
    named: Vec<(usize, usize)>,
}

impl Flat {
    /// A copy of its own for a listener of a channel to `.Every`, counted as a copy is.
    fn copied(&self, state: &mut State) -> Flat {
        let blocks: Vec<Box<[u64]>> = self
            .blocks
            .iter()
            .map(|block| {
                state.heap.copied(block.len());
                block.clone()
            })
            .collect();
        let mut words = self.words.clone();
        for (place, which) in &self.named {
            if let (Some(word), Some(block)) = (words.get_mut(*place), blocks.get(*which)) {
                *word = address_of(block);
            }
        }
        Flat { kind: self.kind, words, blocks, named: self.named.clone() }
    }
}

/// `fennec_channel_send_words(channel, kind, width, done, timeout_ns)`: sends the message of kind
/// `kind`, `width` words wide, whose words are in this thread's buffer. Gives back what
/// `fennec_channel_send` does. With `done` 0 it takes nothing from the sender: each block its words
/// name is copied. `done` 1, handing the sender's owners over, is for a later step, and stops the
/// program as a bug in Fennec until then, as a kind that isn't flat or a width that isn't the
/// kind's does.
pub extern "C" fn channel_send_words(channel: i64, kind: i64, width: i64, done: i64, timeout_ns: i64) -> i64 {
    // First, before anything can make way: another task on this thread may fill the buffer next.
    let taken = with(|state| {
        let (kind, element) = flat_kind(state, kind, width);
        let words = buffered(state, element.words());
        Some((kind, element, words))
    });
    // With no runtime running there's nothing to send on.
    let Some((kind, element, words)) = taken else { return CLOSED };
    if done != 0 {
        heap::runtime_bug("a message's owners were handed over at a send, which isn't built yet");
    }
    // What this task printed comes before anything whoever takes this goes on to print.
    super::super::write_kept_output();
    let Some(channel) = find(channel) else { return CLOSED };
    // In a replay, it waits its turn, and a send that didn't go through doesn't wait again.
    let timeout_ns = match replayed(&channel, "send", timeout_ns < 0) {
        Turn::Live => timeout_ns,
        Turn::Goes => -1,
        Turn::Gives(code) => return code,
    };
    let copied = attempt(|state: &mut State| copied(state, kind, &element, words).map(Some));
    let Some(message) = copied else { return DONE };
    send_on(&channel, Message::Words(Box::new(message)), timeout_ns)
}

/// `fennec_channel_receive_words(channel, kind, width, timeout_ns)`: takes the next message, of
/// kind `kind`, `width` words wide, into this thread's buffer, the blocks its words name now the
/// receiver's, each with one owner, and gives back 0. Gives back what `fennec_channel_receive` does
/// otherwise, but 3, `NO_MESSAGE`, where that gives 0 with no message, as for a stop. A message that
/// was sent as one word, or as another kind, stops the program as a bug in Fennec.
pub extern "C" fn channel_receive_words(channel: i64, kind: i64, width: i64, timeout_ns: i64) -> i64 {
    let Some(kind) = with(|state| Some(flat_kind(state, kind, width).0)) else { return CLOSED };
    match next(channel, timeout_ns) {
        Ok(message) => {
            taken_in(message, kind);
            DONE
        }
        Err(DONE) => NO_MESSAGE,
        Err(code) => code,
    }
}

/// The kind numbered `kind`, checked as flat and `width` words wide: anything else is a bug in the
/// code that asked, which stops the program. A kind the runtime numbered itself, past those the
/// program described, is numbered on each thread, so it means nothing to the receiver: refused.
fn flat_kind(state: &State, kind: i64, width: i64) -> (usize, std::rc::Rc<Element>) {
    let found = usize::try_from(kind).ok().and_then(|at| Some((at, state.tables.elements.get(at)?)));
    let Some((kind, element)) = found else {
        heap::runtime_bug("a message's kind isn't one the program described");
    };
    if !element.is_flat_message() {
        heap::runtime_bug("a message sent or received as words isn't flat");
    }
    if usize::try_from(width).ok() != Some(element.words()) {
        heap::runtime_bug("a message's width isn't its kind's");
    }
    (kind, std::rc::Rc::clone(element))
}

/// The first `count` words of this thread's buffer.
fn buffered(state: &State, count: usize) -> Box<[u64]> {
    super::super::dicts::value_buffer_of(state, |buffer| {
        buffer.get(..count).map(|words| words.iter().map(std::cell::Cell::get).collect())
    })
    .unwrap_or_else(|| heap::runtime_bug("a message is wider than the buffer it's passed in"))
}

/// `words`, a message of `element`, with each block they name copied, the word naming it pointed at
/// its copy: a block two words name is copied once, with two owners. A block every thread can read,
/// a string literal, isn't copied: it travels as its address. A block the sender can't find fails.
fn copied(
    state: &mut State,
    kind: usize,
    element: &Element,
    mut words: Box<[u64]>,
) -> Result<Flat, Diagnostic> {
    let mut places = Vec::new();
    block_places(element, 0, &words, &mut places);
    let mut blocks: Vec<Box<[u64]>> = Vec::new();
    let mut named = Vec::new();
    // Which block each copy was made of, by its place in `blocks`.
    let mut made_of: Vec<u64> = Vec::new();
    for place in places {
        let Some(&address) = words.get(place) else { continue };
        if let Some(which) = made_of.iter().position(|old| *old == address) {
            if let Some(count) = blocks.get_mut(which).and_then(|block| block.first_mut()) {
                *count += 1;
            }
            if let (Some(word), Some(block)) = (words.get_mut(place), blocks.get(which)) {
                *word = address_of(block);
            }
            named.push((place, which));
            continue;
        }
        let block = state.heap.block(address).ok_or_else(|| {
            Diagnostic::new(
                format!("internal error: block {address:#x} of a message isn't there"),
                Span::default(),
            )
        })?;
        if block.first().is_some_and(|count| *count >= IMMORTAL) && heap::shared_static(address) {
            continue;
        }
        let mut copy: Box<[u64]> = block.into();
        if let Some(count) = copy.first_mut() {
            *count = 1;
        }
        state.heap.copied(copy.len());
        if let Some(word) = words.get_mut(place) {
            *word = address_of(&copy);
        }
        named.push((place, blocks.len()));
        made_of.push(address);
        blocks.push(copy);
    }
    Ok(Flat { kind, words, blocks, named })
}

/// The places in `words`, from `at`, of the words of a message of `element` that name a block.
fn block_places(element: &Element, at: usize, words: &[u64], places: &mut Vec<usize>) {
    match element {
        Element::Struct { fields, .. } => {
            let mut field_at = at;
            for (_, field) in fields {
                block_places(field, field_at, words, places);
                field_at += field.words();
            }
        }
        Element::Text => {
            if let Some(header) = words.get(at..at + 3).and_then(|header| <[u64; 3]>::try_from(header).ok())
                && super::super::text::block_of(&header).is_some_and(|block| block != 0)
            {
                places.push(at);
            }
        }
        // Its first word is its case's number, and its case's fields follow; the words past them,
        // up to the widest case's, are zero.
        Element::Enum { cases, .. } => {
            let case = words.get(at).and_then(|number| usize::try_from(*number).ok());
            let Some((_, fields)) = case.and_then(|case| cases.get(case)) else {
                heap::runtime_bug("a message's case isn't one its enum has");
            };
            let mut field_at = at + 1;
            for (_, field) in fields {
                block_places(field, field_at, words, places);
                field_at += field.words();
            }
        }
        // All three words zero is `None`, which `block_of` reads as no block too.
        Element::Maybe(inner) if **inner == Element::Text => block_places(inner, at, words, places),
        Element::Str | Element::Boxed(_) | Element::Maybe(_)
            if words.get(at).is_some_and(|word| *word != 0) =>
        {
            places.push(at);
        }
        _ => {}
    }
}

/// Takes `message` in as the receiver's: its blocks into this thread's heap, marked as its own, and
/// its words into the buffer.
fn taken_in(message: Message, kind: usize) {
    with(|state| {
        let words = match message {
            Message::Words(words) => *words,
            Message::SharedWords(kept) => kept.copied(state),
            Message::Word(_) | Message::Block(..) | Message::Shared(..) => {
                heap::runtime_bug("a message sent as one word was received as words")
            }
        };
        if words.kind != kind {
            heap::runtime_bug("a message was received as a kind it wasn't sent as");
        }
        let Flat { words, blocks, .. } = words;
        state.heap.adopt_made(blocks);
        let filled = super::super::dicts::value_buffer_of(state, |buffer| {
            buffer.get(..words.len()).map(|cells| {
                for (cell, word) in cells.iter().zip(words.iter()) {
                    cell.set(*word);
                }
            })
        });
        if filled.is_none() {
            heap::runtime_bug("a message is wider than the buffer it's passed in");
        }
    });
}

/// A block's address, as a word names it.
fn address_of(block: &[u64]) -> u64 {
    u64::try_from(block.as_ptr().addr()).unwrap_or(0)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod replayed;
