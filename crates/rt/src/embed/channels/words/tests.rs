//! Messages sent as words, on one thread and from the program's thread to a launched task, as
//! generated code will send them. Nothing calls these from a program yet.

use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};

use super::super::{channel_close, channel_free, channel_listen, channel_new, channel_new_every};
use super::*;
use crate::embed::parallel::Pool;
use crate::embed::serve::{launch_end, launch_on};
use crate::embed::{Tables, finish, start};

const FOREVER: i64 = -1;
const MILLISECOND: i64 = 1_000_000;
const PATIENCE: i64 = 10_000 * MILLISECOND;

/// A chat message: who said it, in which room, what, and an optional note.
const MESSAGE: i64 = 1;
/// A line held as a string's header, and where it was.
const LINE: i64 = 2;
/// A list of strings, which isn't flat.
const LIST: i64 = 3;
/// An event, an enum: two numbers, a number and a string by address, a string's header and a
/// number, or nothing.
const EVENT: i64 = 4;
/// A numbered event, a struct holding an enum.
const SAID: i64 = 5;

fn event() -> Element {
    let field = |name: &str, element: Element| (Rc::from(name), element);
    Element::Enum {
        name: Rc::from("Event"),
        cases: vec![
            (Rc::from("Join"), vec![field("user", Element::Int), field("room", Element::Int)]),
            (Rc::from("Say"), vec![field("user", Element::Int), field("text", Element::Str)]),
            (Rc::from("Line"), vec![field("text", Element::Text), field("at", Element::Int)]),
            (Rc::from("Quiet"), vec![]),
        ],
        words: 5,
    }
}

fn tables() -> Tables {
    let string = Element::Str;
    let message = Element::Struct {
        name: Rc::from("Message"),
        fields: vec![
            (Rc::from("user"), Element::Int),
            (Rc::from("room"), Element::Str),
            (Rc::from("text"), Element::Str),
            (Rc::from("note"), Element::Maybe(Rc::new(Element::Str))),
        ],
    };
    let line = Element::Struct {
        name: Rc::from("Line"),
        fields: vec![(Rc::from("text"), Element::Text), (Rc::from("at"), Element::Int)],
    };
    let said = Element::Struct {
        name: Rc::from("Said"),
        fields: vec![(Rc::from("seq"), Element::Int), (Rc::from("event"), event())],
    };
    let elements = vec![
        Rc::new(string),
        Rc::new(message),
        Rc::new(line),
        Rc::new(Element::List(Rc::new(Element::Str))),
        Rc::new(event()),
        Rc::new(said),
    ];
    Tables { shapes: vec![Rc::new(Element::Str)], elements, str_shape: 0, ..Tables::default() }
}

fn begin() {
    start(tables(), crate::Inputs::default());
}

fn pool() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {
            start(tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(2, &setup)
    })
}

fn string(text: &str) -> u64 {
    with(|state| heap::text_address(state, text)).cast_unsigned()
}

fn text(address: u64) -> String {
    match with(|state| Some(heap::read_block(state, address))) {
        Some(crate::value::Value::Str(text)) => text.to_string(),
        other => panic!("not a string: {other:?}"),
    }
}

fn count(address: u64) -> u64 {
    with(|state| state.heap.block(address).and_then(|block| block.first().copied())).unwrap_or(0)
}

fn blocks() -> usize {
    with(|state| state.heap.blocks.len())
}

fn fill(words: &[u64]) {
    with(|state| {
        crate::embed::dicts::value_buffer_of(state, |buffer| {
            for (cell, word) in buffer.iter().zip(words) {
                cell.set(*word);
            }
        });
    });
}

fn filled(count: usize) -> Vec<u64> {
    with(|state| {
        crate::embed::dicts::value_buffer_of(state, |buffer| {
            buffer.iter().take(count).map(std::cell::Cell::get).collect()
        })
    })
}

#[test]
fn a_flat_message_goes_as_copies_and_comes_out_whole() {
    begin();
    let channel = channel_new(4);
    let room = string("the long room name, over twenty-three bytes");
    let said = string("a message long enough to need a block of its own");
    let before = blocks();
    fill(&[7, room, said, 0]);
    assert_eq!(channel_send_words(channel, MESSAGE, 4, 0, FOREVER), DONE);
    // The copies are the channel's, in no heap: the sender's own are as they were.
    assert_eq!(blocks(), before);
    assert_eq!((count(room), count(said)), (1, 1));
    fill(&[0, 0, 0, 0]);
    assert_eq!(channel_receive_words(channel, MESSAGE, 4, FOREVER), DONE);
    let got = filled(4);
    assert_eq!(got.first(), Some(&7));
    assert_eq!(got.get(3), Some(&0), "a `None` note stays `None`");
    let (room_copy, said_copy) = (got[1], got[2]);
    assert!(room_copy != room && said_copy != said, "each block is a copy");
    assert_eq!(text(room_copy), "the long room name, over twenty-three bytes");
    assert_eq!(text(said_copy), "a message long enough to need a block of its own");
    // Now the receiver's, each with one owner.
    assert_eq!(blocks(), before + 2);
    assert_eq!((count(room_copy), count(said_copy)), (1, 1));
    channel_free(channel);
    finish();
}

#[test]
fn a_block_two_words_name_is_copied_once_with_two_owners() {
    begin();
    let channel = channel_new(1);
    let room = string("both the room and the note, long enough for a block");
    fill(&[1, room, room, room]);
    assert_eq!(channel_send_words(channel, MESSAGE, 4, 0, FOREVER), DONE);
    assert_eq!(channel_receive_words(channel, MESSAGE, 4, FOREVER), DONE);
    let got = filled(4);
    assert_ne!(got[1], room);
    assert_eq!((got[2], got[3]), (got[1], got[1]), "one copy, named three times");
    assert_eq!(count(got[1]), 3);
    channel_free(channel);
    finish();
}

#[test]
fn a_header_in_a_block_is_copied_and_one_held_inline_goes_as_it_is() {
    begin();
    let channel = channel_new(2);
    let long =
        with(|state| crate::embed::text::header(state, "a line far longer than a header holds itself"));
    let short = with(|state| crate::embed::text::header(state, "short"));
    fill(&[long[0], long[1], long[2], 3]);
    assert_eq!(channel_send_words(channel, LINE, 4, 0, FOREVER), DONE);
    fill(&[short[0], short[1], short[2], 4]);
    assert_eq!(channel_send_words(channel, LINE, 4, 0, FOREVER), DONE);
    assert_eq!(channel_receive_words(channel, LINE, 4, FOREVER), DONE);
    let got = filled(4);
    assert_ne!(got[0], long[0], "the block is a copy");
    assert_eq!((got[1], got[2], got[3]), (long[1], long[2], 3), "the length and the tag are the same");
    let header = [got[0], got[1], got[2]];
    assert_eq!(
        with(|state| crate::embed::text::text_of(state, &header)),
        "a line far longer than a header holds itself"
    );
    assert_eq!(channel_receive_words(channel, LINE, 4, FOREVER), DONE);
    assert_eq!(filled(4), vec![short[0], short[1], short[2], 4]);
    channel_free(channel);
    finish();
}

#[test]
fn a_send_that_fails_takes_nothing_and_a_freed_channel_leaves_nothing() {
    begin();
    let room = string("a room long enough for a block of its own, again");
    let before = blocks();
    let closed = channel_new(4);
    channel_close(closed);
    fill(&[1, room, room, 0]);
    assert_eq!(channel_send_words(closed, MESSAGE, 4, 0, FOREVER), CLOSED);
    channel_free(closed);
    let full = channel_new(1);
    for _ in 0..2 {
        fill(&[1, room, room, 0]);
        let sent = channel_send_words(full, MESSAGE, 4, 0, MILLISECOND);
        assert!(sent == DONE || sent == super::super::TIMED_OUT);
    }
    channel_free(full);
    assert_eq!(blocks(), before);
    assert_eq!(count(room), 1);
    finish();
}

#[test]
fn each_listener_of_a_channel_to_every_takes_copies_of_its_own() {
    begin();
    let channel = channel_new_every(8);
    let first = channel_listen(channel, 0);
    let second = channel_listen(channel, 0);
    let said = string("one message for every listener, in a block of its own");
    fill(&[5, said, said, 0]);
    assert_eq!(channel_send_words(channel, MESSAGE, 4, 0, FOREVER), DONE);
    assert_eq!(channel_receive_words(first, MESSAGE, 4, FOREVER), DONE);
    let one = filled(4);
    assert_eq!(channel_receive_words(second, MESSAGE, 4, FOREVER), DONE);
    let other = filled(4);
    assert!(one[1] != said && other[1] != said && one[1] != other[1], "three blocks: {one:?} {other:?}");
    assert_eq!((count(one[1]), count(other[1])), (2, 2));
    assert_eq!((text(one[1]), text(other[1])), (text(said), text(said)));
    // Taking from the channel itself, not a listener, ends with no message.
    assert_eq!(channel_receive_words(channel, MESSAGE, 4, FOREVER), NO_MESSAGE);
    channel_free(channel);
    finish();
}

#[test]
fn a_receive_out_of_time_says_so_and_takes_nothing() {
    begin();
    let channel = channel_new(1);
    fill(&[9, 9, 9, 9]);
    assert_eq!(channel_receive_words(channel, MESSAGE, 4, MILLISECOND), super::super::TIMED_OUT);
    channel_free(channel);
    finish();
}

#[test]
fn only_flat_kinds_are_flat() {
    let tables = tables();
    let flat: Vec<bool> = tables.elements.iter().map(|element| element.is_flat_message()).collect();
    assert_eq!(flat, vec![true, true, true, false, true, true]);
    let listing = Element::Enum {
        name: Rc::from("Listing"),
        cases: vec![
            (Rc::from("Empty"), vec![]),
            (Rc::from("Items"), vec![(Rc::from("items"), Element::List(Rc::new(Element::Str)))]),
        ],
        words: 2,
    };
    assert!(!listing.is_flat_message(), "one case that isn't flat makes the enum not flat");
    let names = Element::Enum { name: Rc::from("Names"), cases: vec![(Rc::from("A"), vec![])], words: 1 };
    assert!(names.is_flat_message(), "an enum of only names goes as its case's number");
    assert_eq!(usize::try_from(LIST).ok().and_then(|at| tables.elements.get(at)).map(|e| e.words()), Some(1));
    let bytes = Element::Boxed(usize::try_from(heap::BYTES_SHAPE).unwrap_or(0));
    assert!(bytes.is_flat_message());
    assert!(!Element::Boxed(0).is_flat_message());
    assert!(!Element::Maybe(Rc::new(Element::Int)).is_flat_message());
}

static INBOX: AtomicI64 = AtomicI64::new(0);
static USERS: AtomicI64 = AtomicI64::new(0);
static LETTERS: AtomicI64 = AtomicI64::new(0);

/// Receives messages as words until its inbox is closed, adding up who sent each and how long the
/// text was, as read on its own thread.
extern "C" fn reads(_env: i64, _thread: i64) -> i64 {
    let inbox = INBOX.load(Ordering::SeqCst);
    while channel_receive_words(inbox, MESSAGE, 4, PATIENCE) == DONE {
        let got = filled(4);
        USERS.fetch_add(got[0].cast_signed(), Ordering::SeqCst);
        LETTERS.fetch_add(i64::try_from(text(got[2]).len()).unwrap_or(0), Ordering::SeqCst);
    }
    0
}

#[test]
fn a_task_on_another_thread_receives_messages_sent_as_words() {
    begin();
    let inbox = channel_new(2);
    INBOX.store(inbox, Ordering::SeqCst);
    let task = launch_on(pool(), reads, 0);
    let said = string("forty letters of text, no more, no less!");
    for user in 1..=50 {
        fill(&[user, said, said, 0]);
        assert_eq!(channel_send_words(inbox, MESSAGE, 4, 0, PATIENCE), DONE);
    }
    channel_close(inbox);
    let mut failure = 0;
    assert_eq!(launch_end(task, FOREVER, 0, &mut failure), 0);
    channel_free(inbox);
    assert_eq!(USERS.load(Ordering::SeqCst), (1..=50).sum::<i64>());
    assert_eq!(LETTERS.load(Ordering::SeqCst), 50 * 40);
    assert_eq!(count(said), 1, "the sender's own is as it was");
    finish();
}

#[test]
fn an_enum_goes_as_words_and_only_its_case_s_blocks_are_copied() {
    begin();
    let channel = channel_new(4);
    let said = string("a string long enough to need a block of its own, said");
    let long =
        with(|state| crate::embed::text::header(state, "a line in a header, far longer than one holds"));
    let before = blocks();
    // `Join` holds two numbers: a word that would name a block in `Say` is a number here.
    fill(&[0, 3, said, 0, 0]);
    assert_eq!(channel_send_words(channel, EVENT, 5, 0, FOREVER), DONE);
    fill(&[1, 4, said, 0, 0]);
    assert_eq!(channel_send_words(channel, EVENT, 5, 0, FOREVER), DONE);
    fill(&[2, long[0], long[1], long[2], 9]);
    assert_eq!(channel_send_words(channel, EVENT, 5, 0, FOREVER), DONE);
    fill(&[3, 0, 0, 0, 0]);
    assert_eq!(channel_send_words(channel, EVENT, 5, 0, FOREVER), DONE);
    assert_eq!(blocks(), before);
    assert_eq!(count(said), 1);
    assert_eq!(channel_receive_words(channel, EVENT, 5, FOREVER), DONE);
    assert_eq!(filled(5), vec![0, 3, said, 0, 0], "nothing of `Join`'s is copied");
    assert_eq!(channel_receive_words(channel, EVENT, 5, FOREVER), DONE);
    let say = filled(5);
    assert_eq!((say[0], say[1]), (1, 4));
    assert_ne!(say[2], said, "the string is a copy");
    assert_eq!(text(say[2]), "a string long enough to need a block of its own, said");
    assert_eq!(count(say[2]), 1);
    assert_eq!(channel_receive_words(channel, EVENT, 5, FOREVER), DONE);
    let line = filled(5);
    assert_eq!(line[0], 2);
    assert_ne!(line[1], long[0], "the header's block is a copy");
    assert_eq!((line[2], line[3], line[4]), (long[1], long[2], 9));
    let header = [line[1], line[2], line[3]];
    assert_eq!(
        with(|state| crate::embed::text::text_of(state, &header)),
        "a line in a header, far longer than one holds"
    );
    assert_eq!(channel_receive_words(channel, EVENT, 5, FOREVER), DONE);
    assert_eq!(filled(5), vec![3, 0, 0, 0, 0]);
    assert_eq!(blocks(), before + 2, "a copy for `Say` and one for `Line`, now the receiver's");
    channel_free(channel);
    finish();
}

#[test]
fn a_struct_holding_an_enum_copies_the_blocks_of_the_case_it_holds() {
    begin();
    let channel = channel_new(2);
    let said = string("a string long enough to need a block, in a struct's enum");
    fill(&[7, 1, 4, said, 0, 0]);
    assert_eq!(channel_send_words(channel, SAID, 6, 0, FOREVER), DONE);
    fill(&[8, 0, 5, said, 0, 0]);
    assert_eq!(channel_send_words(channel, SAID, 6, 0, FOREVER), DONE);
    assert_eq!(channel_receive_words(channel, SAID, 6, FOREVER), DONE);
    let got = filled(6);
    assert_eq!((got[0], got[1], got[2]), (7, 1, 4));
    assert_ne!(got[3], said);
    assert_eq!(text(got[3]), "a string long enough to need a block, in a struct's enum");
    assert_eq!(channel_receive_words(channel, SAID, 6, FOREVER), DONE);
    assert_eq!(filled(6), vec![8, 0, 5, said, 0, 0], "`Join`'s second number goes as it is");
    assert_eq!(count(said), 1);
    channel_free(channel);
    finish();
}

#[test]
fn each_listener_of_a_channel_to_every_takes_its_own_copy_of_an_enum_s_blocks() {
    begin();
    let channel = channel_new_every(4);
    let first = channel_listen(channel, 0);
    let second = channel_listen(channel, 0);
    let said = string("an event for every listener, in a block of its own");
    fill(&[1, 6, said, 0, 0]);
    assert_eq!(channel_send_words(channel, EVENT, 5, 0, FOREVER), DONE);
    assert_eq!(channel_receive_words(first, EVENT, 5, FOREVER), DONE);
    let one = filled(5);
    assert_eq!(channel_receive_words(second, EVENT, 5, FOREVER), DONE);
    let other = filled(5);
    assert!(one[2] != said && other[2] != said && one[2] != other[2], "three blocks: {one:?} {other:?}");
    assert_eq!((text(one[2]), text(other[2])), (text(said), text(said)));
    channel_free(channel);
    finish();
}

#[test]
#[should_panic(expected = "a message's case isn't one its enum has")]
fn a_case_its_enum_does_not_have_stops_the_program() {
    // Through `fennec_channel_send_words` it stops the program, which a test can't catch.
    let mut places = Vec::new();
    block_places(&event(), 0, &[4, 0, 0, 0, 0], &mut places);
}
