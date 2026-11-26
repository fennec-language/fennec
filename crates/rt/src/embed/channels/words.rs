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
///
/// It's made on the sender's thread and let go of on the receiver's, so each allocation it holds is
/// one the sender's allocator later takes back from another thread (#2492): it holds only its words
/// and its copies, and travels unboxed. Its words are in it when they fit, as a message's mostly
/// do, and so is its first copy: a message of a few fields and a string takes no allocation but the
/// string's copy. Boxed, they cost `chat_server` an allocation and its free on another thread for
/// each message, and another for each list of copies.
pub(in crate::embed) struct Flat {
    /// Its kind's number in the program's table of kinds.
    kind: usize,
    words: Words,
    blocks: Copies,
}

/// The most words a message holds in itself: a few fields, or two strings and two numbers.
const WORDS_IN_PLACE: usize = 8;

/// A message's words: in place when there are `WORDS_IN_PLACE` or fewer, and boxed otherwise.
#[derive(Clone)]
enum Words {
    InPlace { count: usize, words: [u64; WORDS_IN_PLACE] },
    Boxed(Box<[u64]>),
}

impl Words {
    /// The words `each` gives, `count` of them.
    fn of(count: usize, each: impl Iterator<Item = u64>) -> Words {
        if count <= WORDS_IN_PLACE {
            let mut words = [0; WORDS_IN_PLACE];
            for (word, given) in words.iter_mut().zip(each) {
                *word = given;
            }
            return Words::InPlace { count, words };
        }
        Words::Boxed(each.take(count).collect())
    }
}

impl std::ops::Deref for Words {
    type Target = [u64];
    fn deref(&self) -> &[u64] {
        match self {
            Words::InPlace { count, words } => words.get(..*count).unwrap_or_default(),
            Words::Boxed(words) => words,
        }
    }
}

impl std::ops::DerefMut for Words {
    fn deref_mut(&mut self) -> &mut [u64] {
        match self {
            Words::InPlace { count, words } => words.get_mut(..*count).unwrap_or_default(),
            Words::Boxed(words) => words,
        }
    }
}

/// The copies of the blocks a message names, the first held in place, so one naming a single block,
/// as most that name any do, makes no list.
#[derive(Default)]
struct Copies {
    first: Option<Box<[u64]>>,
    rest: Vec<Box<[u64]>>,
}

impl Copies {
    fn push(&mut self, block: Box<[u64]>) {
        if self.first.is_none() {
            self.first = Some(block);
        } else {
            self.rest.push(block);
        }
    }

    fn iter(&self) -> impl Iterator<Item = &[u64]> {
        self.first.iter().chain(&self.rest).map(|block| &**block)
    }

    fn get(&self, at: usize) -> Option<&[u64]> {
        match at {
            0 => self.first.as_deref(),
            _ => self.rest.get(at - 1).map(|block| &**block),
        }
    }

    fn get_mut(&mut self, at: usize) -> Option<&mut [u64]> {
        match at {
            0 => self.first.as_deref_mut(),
            _ => self.rest.get_mut(at - 1).map(|block| &mut **block),
        }
    }
}

impl IntoIterator for Copies {
    type Item = Box<[u64]>;
    type IntoIter = std::iter::Chain<std::option::IntoIter<Box<[u64]>>, std::vec::IntoIter<Box<[u64]>>>;
    fn into_iter(self) -> Self::IntoIter {
        self.first.into_iter().chain(self.rest)
    }
}

impl Flat {
    /// A copy of its own for a listener of a channel to `.Every`, counted as a copy is: each word
    /// that names one of its copies names that one's copy.
    fn copied(&self, state: &mut State) -> Flat {
        let mut blocks = Copies::default();
        for block in self.blocks.iter() {
            state.heap.copied(block.len());
            blocks.push(Box::from(block));
        }
        let mut places = Vec::new();
        each_place(&plan_of(state, self.kind).parts, &self.words, &mut |place| places.push(place));
        let mut words = self.words.clone();
        for place in places {
            let Some(word) = words.get_mut(place) else { continue };
            // A word that names none of them names a block every thread can read, which wasn't copied.
            if let Some(copy) =
                self.blocks.iter().position(|block| address_of(block) == *word).and_then(|at| blocks.get(at))
            {
                *word = address_of(copy);
            }
        }
        Flat { kind: self.kind, words, blocks }
    }
}

/// Where the words of a flat message of one kind may name blocks, and how many words it takes:
/// worked out from its type once a run on each thread, the first time a message is sent or taken
/// there, so a send or a receive reads it rather than walking the type again (#2492).
pub(in crate::embed) struct Plan {
    width: usize,
    parts: Box<[Part]>,
}

/// One place in a flat message's words where a block may be named.
enum Part {
    /// A string's three-word header at this word, naming a block unless its bytes are in place.
    Text(usize),
    /// A word naming a block, or 0 for none.
    Block(usize),
    /// An enum's case number at this word, and where each case's fields may name blocks.
    Cases(usize, Box<[Box<[Part]>]>),
}

/// The plan of every kind the run has, `None` for one that isn't flat: made the first time this
/// thread's runtime sends or takes a message as words.
fn plans(state: &State) -> &[Option<Plan>] {
    state.message_plans.get_or_init(|| {
        let planned = |element: &std::rc::Rc<Element>| {
            element.is_flat_message().then(|| {
                let mut parts = Vec::new();
                parts_of(element, 0, &mut parts);
                Plan { width: element.words(), parts: parts.into_boxed_slice() }
            })
        };
        state.tables.elements.iter().map(planned).collect()
    })
}

/// The plan of kind `kind`, which the caller checked is flat: anything else is a bug in Fennec.
fn plan_of(state: &State, kind: usize) -> &Plan {
    let Some(Some(plan)) = plans(state).get(kind) else {
        heap::runtime_bug("a message's kind isn't a flat one the program described");
    };
    plan
}

/// Into `parts`, where the words of a value of `element`, from word `at`, may name blocks.
fn parts_of(element: &Element, at: usize, parts: &mut Vec<Part>) {
    match element {
        Element::Struct { fields, .. } => {
            let mut field_at = at;
            for (_, field) in fields {
                parts_of(field, field_at, parts);
                field_at += field.words();
            }
        }
        Element::Text => parts.push(Part::Text(at)),
        // Its first word is its case's number, and its case's fields follow; the words past them,
        // up to the widest case's, are zero.
        Element::Enum { cases, .. } => {
            let cases = cases
                .iter()
                .map(|(_, fields)| {
                    let mut case = Vec::new();
                    let mut field_at = at + 1;
                    for (_, field) in fields {
                        parts_of(field, field_at, &mut case);
                        field_at += field.words();
                    }
                    case.into_boxed_slice()
                })
                .collect();
            parts.push(Part::Cases(at, cases));
        }
        // All three words zero is `None`, which `block_of` reads as no block too.
        Element::Maybe(inner) if **inner == Element::Text => parts.push(Part::Text(at)),
        Element::Str | Element::Boxed(_) | Element::Maybe(_) => parts.push(Part::Block(at)),
        _ => {}
    }
}

/// Calls `each` with the place in `words` of each word that names a block, as `parts` says.
fn each_place(parts: &[Part], words: &[u64], each: &mut dyn FnMut(usize)) {
    for part in parts {
        match part {
            Part::Text(at) => {
                if let Some(header) =
                    words.get(*at..at + 3).and_then(|header| <[u64; 3]>::try_from(header).ok())
                    && super::super::text::block_of(&header).is_some_and(|block| block != 0)
                {
                    each(*at);
                }
            }
            Part::Block(at) => {
                if words.get(*at).is_some_and(|word| *word != 0) {
                    each(*at);
                }
            }
            Part::Cases(at, cases) => {
                let case = words.get(*at).and_then(|number| usize::try_from(*number).ok());
                let Some(fields) = case.and_then(|case| cases.get(case)) else {
                    heap::runtime_bug("a message's case isn't one its enum has");
                };
                each_place(fields, words, each);
            }
        }
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
        let kind = flat_kind(state, kind, width);
        Some((kind, buffered(state, plan_of(state, kind).width)))
    });
    // With no runtime running there's nothing to send on.
    let Some((kind, words)) = taken else { return CLOSED };
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
    let copied = attempt(|state: &mut State| copied(state, kind, words).map(Some));
    let Some(message) = copied else { return DONE };
    send_on(&channel, Message::Words(message), timeout_ns)
}

/// `fennec_channel_receive_words(channel, kind, width, timeout_ns)`: takes the next message, of
/// kind `kind`, `width` words wide, into this thread's buffer, the blocks its words name now the
/// receiver's, each with one owner, and gives back 0. Gives back what `fennec_channel_receive` does
/// otherwise, but 3, `NO_MESSAGE`, where that gives 0 with no message, as for a stop. A message that
/// was sent as one word, or as another kind, stops the program as a bug in Fennec.
pub extern "C" fn channel_receive_words(channel: i64, kind: i64, width: i64, timeout_ns: i64) -> i64 {
    let Some(kind) = with(|state| Some(flat_kind(state, kind, width))) else { return CLOSED };
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
fn flat_kind(state: &State, kind: i64, width: i64) -> usize {
    let found = usize::try_from(kind).ok().and_then(|at| Some((at, plans(state).get(at)?)));
    let Some((kind, plan)) = found else {
        heap::runtime_bug("a message's kind isn't one the program described");
    };
    let Some(plan) = plan else {
        heap::runtime_bug("a message sent or received as words isn't flat");
    };
    if usize::try_from(width).ok() != Some(plan.width) {
        heap::runtime_bug("a message's width isn't its kind's");
    }
    kind
}

/// The first `count` words of this thread's buffer.
fn buffered(state: &State, count: usize) -> Words {
    super::super::dicts::value_buffer_of(state, |buffer| {
        buffer.get(..count).map(|words| Words::of(count, words.iter().map(std::cell::Cell::get)))
    })
    .unwrap_or_else(|| heap::runtime_bug("a message is wider than the buffer it's passed in"))
}

/// `words`, a message of kind `kind`, with each block they name copied, the word naming it pointed
/// at its copy: a block two words name is copied once, with two owners. A block every thread can
/// read, a string literal, isn't copied: it travels as its address. A block the sender can't find
/// fails. Where the blocks are named, and which each copy was made of, are kept in this thread's
/// own lists between sends, so a send makes only the copies and the list that holds them.
fn copied(state: &mut State, kind: usize, mut words: Words) -> Result<Flat, Diagnostic> {
    let (mut places, mut made_of) = std::mem::take(&mut state.message_lists);
    places.clear();
    made_of.clear();
    each_place(&plan_of(state, kind).parts, &words, &mut |place| places.push(place));
    let copies = copies(state, &places, &mut words, &mut made_of);
    state.message_lists = (places, made_of);
    Ok(Flat { kind, words, blocks: copies? })
}

/// The copies of the blocks the words at `places` in `words` name, each word pointed at its copy,
/// with `made_of` the block each copy was made of, by its place in the copies.
fn copies(
    state: &mut State,
    places: &[usize],
    words: &mut [u64],
    made_of: &mut Vec<u64>,
) -> Result<Copies, Diagnostic> {
    let mut blocks = Copies::default();
    for place in places {
        let Some(&address) = words.get(*place) else { continue };
        if let Some(which) = made_of.iter().position(|old| *old == address) {
            if let Some(count) = blocks.get_mut(which).and_then(|block| block.first_mut()) {
                *count += 1;
            }
            if let (Some(word), Some(block)) = (words.get_mut(*place), blocks.get(which)) {
                *word = address_of(block);
            }
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
        if let Some(word) = words.get_mut(*place) {
            *word = address_of(&copy);
        }
        made_of.push(address);
        blocks.push(copy);
    }
    Ok(blocks)
}

/// Takes `message` in as the receiver's: its blocks into this thread's heap, marked as its own, and
/// its words into the buffer.
fn taken_in(message: Message, kind: usize) {
    with(|state| {
        let words = match message {
            Message::Words(words) => words,
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
