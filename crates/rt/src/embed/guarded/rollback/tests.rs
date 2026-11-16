//! Each test does what generated code will: takes a lock, begins its log, saves each word before
//! writing it, and at the end keeps the changes or puts them back, as native code would on a crash.

use std::rc::Rc;

use super::super::{shared_enter, shared_leave, shared_new};
use super::*;
use crate::embed::lists::LIST_HEADER_WORDS;
use crate::embed::{Tables, finish, lists, start, with};
use crate::packed::Element;

/// The shape number of a struct with a list of strings and a number.
const LOG: i64 = 0;
/// The kind of item of a list of strings.
const LINES: i64 = 0;
/// Where a `Log`'s list and number are among its words.
const LINES_AT: usize = heap::HEADER_WORDS;
const COUNT_AT: usize = heap::HEADER_WORDS + 1;

fn tables() -> Tables {
    let strings = Rc::new(Element::Str);
    let log = Element::Struct {
        name: Rc::from("Log"),
        fields: vec![
            (Rc::from("lines"), Element::List(Rc::clone(&strings))),
            (Rc::from("count"), Element::Int),
        ],
    };
    Tables {
        shapes: vec![Rc::new(log), Rc::new(Element::Str)],
        elements: vec![strings],
        str_shape: 1,
        ..Tables::default()
    }
}

fn begin_run() {
    start(tables(), crate::Inputs::default());
}

fn word(block: i64, at: usize) -> i64 {
    let found = with(|state| state.heap.block(u64::try_from(block).unwrap()).map(|words| words[at]));
    i64::try_from(found.unwrap_or(0)).unwrap()
}

fn set_word(block: i64, at: usize, value: i64) {
    with(|state| {
        if let Some(words) = state.heap.block_mut(u64::try_from(block).unwrap()) {
            words[at] = u64::try_from(value).unwrap();
        }
    });
}

/// Lets go of one count of a block.
fn release(block: i64) {
    with(|state| heap::release(state, u64::try_from(block).unwrap()));
}

fn count_of(block: i64) -> i64 {
    word(block, 0)
}

fn exists(block: i64) -> bool {
    with(|state| state.heap.block(u64::try_from(block).unwrap()).is_some())
}

/// How many blocks the current table has.
fn blocks() -> usize {
    with(|state| state.heap.blocks.len())
}

/// What a list of strings holds.
fn lines(list: i64) -> Vec<String> {
    let read = with(|state| Some(heap::read_block(state, u64::try_from(list).unwrap()))).unwrap();
    let crate::value::Value::List(items) = read else { panic!("not a list") };
    items
        .iter()
        .map(|item| match item {
            crate::value::Value::Str(text) => text.to_string(),
            other => panic!("not a string: {other:?}"),
        })
        .collect()
}

fn list(items: &[&str]) -> i64 {
    with(|state| lists::list_of_strings(state, LINES, items))
}

/// A string's own block.
fn string(text: &str) -> i64 {
    let held = list(&[text]);
    let item = word(held, LIST_HEADER_WORDS);
    with(|state| heap::retain(state, u64::try_from(item).unwrap()));
    heap::free(held);
    item
}

/// A `shared` value holding a `Log` of these lines and the number 3, locked to write, with its log
/// begun. Gives back its cell.
fn locked(items: &[&str]) -> i64 {
    let held = list(items);
    let cell = heap::alloc(LOG);
    set_word(cell, LINES_AT, held);
    set_word(cell, COUNT_AT, 3);
    shared_new(cell);
    shared_enter(cell, 1);
    begin(cell);
    cell
}

/// What native code does on a crash: the log's words go back, then the cell's, a word at a time.
fn crash(cell: i64) {
    undo();
    for (at, holds) in [(LINES_AT, 1), (COUNT_AT, 0)] {
        let back = undo_word(i64::try_from(at).unwrap(), word(cell, at), holds);
        set_word(cell, at, back);
    }
}

/// The changes a body makes before it ends: a line changed in place, the number, and the list
/// replaced by a longer one. Gives back the list and the line taken out.
fn change(cell: i64) -> (i64, i64) {
    let inside = word(cell, LINES_AT);
    let line = word(inside, LIST_HEADER_WORDS);
    save(inside, i64::try_from(LIST_HEADER_WORDS).unwrap(), line, 1);
    set_word(inside, LIST_HEADER_WORDS, string("changed"));
    save(cell, i64::try_from(COUNT_AT).unwrap(), 3, 0);
    set_word(cell, COUNT_AT, 4);
    save(cell, i64::try_from(LINES_AT).unwrap(), inside, 1);
    set_word(cell, LINES_AT, list(&["changed", "and", "longer"]));
    (inside, line)
}

#[test]
fn a_crash_puts_back_all_the_lock_changed_and_lets_go_of_what_it_made() {
    begin_run();
    let cell = locked(&["one", "two"]);
    let before = blocks();
    let (inside, line) = change(cell);
    assert!(blocks() > before);
    crash(cell);
    assert_eq!(word(cell, LINES_AT), inside);
    assert_eq!(word(cell, COUNT_AT), 3);
    assert_eq!(word(inside, LIST_HEADER_WORDS), line);
    assert_eq!(lines(inside), ["one", "two"]);
    assert_eq!((count_of(inside), count_of(line)), (1, 1), "each is held once, by the value");
    assert_eq!(blocks(), before, "what the body made is gone");
    assert_eq!(held(), 0);
    shared_leave(cell, 1);
    shared_enter(cell, 1);
    assert_eq!(lines(word(cell, LINES_AT)), ["one", "two"]);
    shared_leave(cell, 1);
    finish();
}

#[test]
fn a_normal_end_keeps_the_changes_and_lets_go_of_what_was_taken_out() {
    begin_run();
    let cell = locked(&["one", "two"]);
    let before = blocks();
    let (inside, line) = change(cell);
    kept();
    assert_eq!(held(), 0);
    assert!(!exists(inside), "the list taken out is gone");
    assert!(!exists(line), "the line taken out is gone");
    let now = word(cell, LINES_AT);
    assert_eq!(lines(now), ["changed", "and", "longer"]);
    assert_eq!(word(cell, COUNT_AT), 4);
    assert_eq!(count_of(now), 1);
    // Three strings and a list for two, against a list of three strings.
    assert_eq!(blocks(), before + 1);
    shared_leave(cell, 1);
    finish();
}

/// Writing one place over and over costs one entry, however long the loop: a log entry for each
/// write once took a program past 4 GB in a few seconds.
#[test]
fn a_word_written_again_and_again_is_saved_once() {
    begin_run();
    let cell = locked(&["one"]);
    let held = string("the first");
    for turn in 0..1000 {
        save(cell, COUNT_AT.try_into().unwrap(), turn, 0);
    }
    assert_eq!(saved_entries(), 1);
    save(cell, LINES_AT.try_into().unwrap(), held, 1);
    let later = string("written under the lock");
    save(cell, LINES_AT.try_into().unwrap(), later, 1);
    assert_eq!(saved_entries(), 2);
    // The second write's old value isn't what a crash puts back, so nothing keeps it.
    assert!(!exists(later));
    assert!(exists(held));
    kept();
    assert_eq!(saved_entries(), 0);
    finish();
}

#[test]
fn a_word_changed_twice_goes_back_to_what_it_was_before_the_lock() {
    begin_run();
    let cell = locked(&["one"]);
    let before = blocks();
    let first = word(cell, LINES_AT);
    let at = i64::try_from(LINES_AT).unwrap();
    save(cell, at, first, 1);
    let second = list(&["second"]);
    set_word(cell, LINES_AT, second);
    save(cell, at, second, 1);
    set_word(cell, LINES_AT, list(&["third"]));
    crash(cell);
    assert_eq!(word(cell, LINES_AT), first);
    assert_eq!(lines(first), ["one"]);
    assert!(!exists(second), "what the log held between is let go of");
    assert_eq!(blocks(), before);
    shared_leave(cell, 1);
    finish();
}

#[test]
fn a_list_changed_where_it_is_is_marked_and_a_copy_of_it_goes_on_a_crash() {
    begin_run();
    let cell = locked(&["one"]);
    let before = blocks();
    let inside = word(cell, LINES_AT);
    let at = i64::try_from(LINES_AT).unwrap();
    save_ref(cell, at, inside);
    let marked = i64::try_from(lists::MARKED).unwrap();
    assert_eq!(count_of(inside), marked + 1, "the log marks it rather than hold it");
    // As an `insert` would, finding it marked: a copy with one more line, in its place.
    let copy = list(&["zero", "one"]);
    release(inside);
    set_word(cell, LINES_AT, copy);
    save_ref(cell, at, copy);
    assert_eq!(count_of(copy), 1, "the copy is the lock's own, so changing it again changes it in place");
    crash(cell);
    assert_eq!(word(cell, LINES_AT), inside);
    assert_eq!(count_of(inside), 1);
    assert!(!exists(copy));
    assert_eq!(blocks(), before);
    shared_leave(cell, 1);
    finish();
}

#[test]
fn an_append_and_a_pop_in_place_go_back_on_a_crash_and_stay_at_an_end() {
    begin_run();
    for crashed in [false, true] {
        let cell = locked(&["one", "two"]);
        let before = blocks();
        let inside = word(cell, LINES_AT);
        save_ref(cell, i64::try_from(LINES_AT).unwrap(), inside);
        // A pop, as native code makes it once the runtime says where: the last item's word comes
        // out with the list's owner of it, which the popping code lets go of.
        assert_eq!(lists::pop_unshare(inside), inside, "a marked list with one owner pops in place");
        let popped = word(inside, LIST_HEADER_WORDS + 1);
        set_word(inside, 2, 1);
        release(popped);
        // An append, filled with a new line.
        let grown = lists::grow(inside);
        set_word(grown, LIST_HEADER_WORDS + 1, string("three"));
        set_word(cell, LINES_AT, grown);
        assert_eq!(lines(grown), ["one", "three"]);
        if crashed {
            crash(cell);
        } else {
            kept();
        }
        let now = word(cell, LINES_AT);
        let wanted: &[&str] = if crashed { &["one", "two"] } else { &["one", "three"] };
        assert_eq!(lines(now), wanted, "crashed: {crashed}");
        assert_eq!(count_of(now), 1, "crashed: {crashed}");
        assert_eq!(blocks(), before, "crashed: {crashed}");
        shared_leave(cell, 1);
    }
    finish();
}

/// Items put in at the front and in the middle of a marked list, with an append between, are made
/// in place, moving the list once it needs room: a crash takes them all out again, newest first,
/// and a normal end keeps them.
#[test]
fn inserts_in_place_go_back_on_a_crash_and_stay_at_an_end() {
    begin_run();
    for crashed in [false, true] {
        let cell = locked(&["one", "two"]);
        let before = blocks();
        let inside = word(cell, LINES_AT);
        save_ref(cell, i64::try_from(LINES_AT).unwrap(), inside);
        // Each as native code makes it: the runtime gives back the list with a zeroed item at the
        // place, which is filled with a new line.
        let (mut now, mut length) = (inside, 2);
        for (at, line) in [(Some(0), "zero"), (None, "three"), (Some(2), "middle")] {
            now = match at {
                Some(at) => lists::insert(now, at, 0, 0),
                None => lists::grow(now),
            };
            let place = at.map_or(length, |at| usize::try_from(at).unwrap());
            length += 1;
            set_word(now, LIST_HEADER_WORDS + place, string(line));
            set_word(cell, LINES_AT, now);
        }
        assert_eq!(lines(now), ["zero", "one", "middle", "two", "three"]);
        assert_eq!(
            count_of(now) & lists::MARKED.cast_signed(),
            lists::MARKED.cast_signed(),
            "changed in place"
        );
        if crashed {
            crash(cell);
        } else {
            kept();
        }
        let after = word(cell, LINES_AT);
        let wanted: &[&str] =
            if crashed { &["one", "two"] } else { &["zero", "one", "middle", "two", "three"] };
        assert_eq!(lines(after), wanted, "crashed: {crashed}");
        assert_eq!(count_of(after), 1, "crashed: {crashed}");
        assert_eq!(blocks(), before + usize::from(!crashed) * 3, "crashed: {crashed}");
        shared_leave(cell, 1);
    }
    finish();
}

#[test]
fn an_item_set_in_place_goes_back_on_a_crash_and_stays_at_an_end() {
    begin_run();
    for crashed in [false, true] {
        let cell = locked(&["one", "two"]);
        let before = blocks();
        let inside = word(cell, LINES_AT);
        save_ref(cell, i64::try_from(LINES_AT).unwrap(), inside);
        // Two sets of the last item, as native code makes them once the runtime says where: the new
        // line goes in, and the list's owner of the old one is let go of.
        for line in ["three", "four"] {
            assert_eq!(
                lists::item_unshare(inside, -1),
                inside,
                "a marked list with one owner is set in place"
            );
            let old = word(inside, LIST_HEADER_WORDS + 1);
            set_word(inside, LIST_HEADER_WORDS + 1, string(line));
            release(old);
        }
        if crashed {
            crash(cell);
        } else {
            kept();
        }
        let now = word(cell, LINES_AT);
        assert_eq!(now, inside, "crashed: {crashed}");
        let wanted: &[&str] = if crashed { &["one", "two"] } else { &["one", "four"] };
        assert_eq!(lines(now), wanted, "crashed: {crashed}");
        assert_eq!(count_of(now), 1, "crashed: {crashed}");
        assert_eq!(blocks(), before, "crashed: {crashed}");
        shared_leave(cell, 1);
    }
    finish();
}

#[test]
fn a_value_passed_to_be_changed_and_not_changed_is_held_once_after_either_end() {
    begin_run();
    for crashed in [false, true] {
        let cell = locked(&["one"]);
        let inside = word(cell, LINES_AT);
        save_ref(cell, i64::try_from(LINES_AT).unwrap(), inside);
        if crashed {
            crash(cell);
        } else {
            kept();
        }
        assert_eq!(word(cell, LINES_AT), inside);
        assert_eq!(count_of(inside), 1, "crashed: {crashed}");
        shared_leave(cell, 1);
    }
    finish();
}

#[test]
fn what_an_inner_lock_changed_of_the_outer_value_goes_back_on_the_outer_crash() {
    begin_run();
    let outer = locked(&["outer"]);
    let outer_list = word(outer, LINES_AT);
    let line = word(outer_list, LIST_HEADER_WORDS);
    let inner = locked(&["inner"]);
    // The outer's number, in its own cell, and a line of its list, in its table behind the inner's.
    save(outer, i64::try_from(COUNT_AT).unwrap(), 3, 0);
    set_word(outer, COUNT_AT, 9);
    save(outer_list, i64::try_from(LIST_HEADER_WORDS).unwrap(), line, 1);
    set_word(outer_list, LIST_HEADER_WORDS, string("changed"));
    // And the inner's own number, which the inner's normal end keeps.
    save(inner, i64::try_from(COUNT_AT).unwrap(), 3, 0);
    set_word(inner, COUNT_AT, 5);
    kept();
    shared_leave(inner, 1);
    assert_eq!(held(), 1);
    assert_eq!(count_of(line), 1, "the outer's log holds what was taken out of it");
    crash(outer);
    assert_eq!(word(outer, COUNT_AT), 3);
    assert_eq!(word(outer_list, LIST_HEADER_WORDS), line);
    assert_eq!(lines(outer_list), ["outer"]);
    shared_leave(outer, 1);
    shared_enter(inner, 1);
    assert_eq!(word(inner, COUNT_AT), 5, "the inner kept its change");
    shared_leave(inner, 1);
    finish();
}

#[test]
fn a_string_s_header_is_held_only_when_it_holds_a_block() {
    begin_run();
    let cell = locked(&[]);
    let block = string("a string long enough to be in a block of its own");
    let inline = 0x83_i64 << 56;
    save_text(cell, 2, block, 11, 0);
    save_text(cell, 5, 0x6f_6f66, 0, inline);
    let mut held: Vec<(usize, bool)> = saves().iter().map(|saved| (saved.word, saved.held)).collect();
    held.sort_unstable();
    assert_eq!(held, [(2, true), (3, false), (4, false), (5, false), (6, false), (7, false)]);
    kept();
    assert!(!exists(block), "the header's block was the log's to let go of");
    shared_leave(cell, 1);
    finish();
}

#[test]
fn a_save_with_no_lock_that_may_crash_lets_go_as_the_write_would() {
    begin_run();
    let line = string("held by nothing else once it's overwritten");
    save(0, 0, line, 1);
    assert!(!exists(line));
    assert_eq!(held(), 0);
    finish();
}

/// A word with its top bit set goes back as it was: a negative number in the cell, and the last
/// word of an inline string's header, whose top bit says it isn't in a block.
#[test]
fn a_word_with_its_top_bit_set_goes_back_as_it_was() {
    begin_run();
    let cell = locked(&["one"]);
    let at = i64::try_from(COUNT_AT).unwrap();
    save(cell, at, -1, 0);
    undo();
    assert_eq!(undo_word(at, 7, 0), -1);
    shared_leave(cell, 1);
    shared_enter(cell, 1);
    begin(cell);
    // `"/a"` inline: its bytes, then 0x80 and its length in the last byte.
    let tag = (0x82_u64 << 56).cast_signed();
    save_text(cell, at, 0x612f, 0, tag);
    undo();
    assert_eq!([undo_word(at, 0, 0), undo_word(at + 1, 0, 0), undo_word(at + 2, 0, 0)], [0x612f, 0, tag]);
    shared_leave(cell, 1);
    finish();
}

/// Every save the logs hold, plain ones in the buffer after the rest.
fn saves() -> Vec<Saved> {
    with_logs(|logs| {
        let mut saves: Vec<Saved> = logs.held.iter().flat_map(|log| log.saved.iter().copied()).collect();
        saves.extend((0..logs.buffer.len()).filter_map(|at| logs.buffer.get(at)));
        saves
    })
}

/// What native code does at a plain save: with room in the buffer, it writes the save there itself,
/// with no call; with none, it calls `save`, which makes room.
fn save_as_native_code(block: i64, word: usize, old: i64) {
    let room = LOGS.with_borrow(|logs| logs.buffer.len() < logs.buffer.room());
    if !room {
        save(block, i64::try_from(word).unwrap(), old, 0);
        return;
    }
    LOGS.with_borrow_mut(|logs| {
        let saved = Saved {
            block: address(block),
            word,
            old: old.cast_unsigned(),
            held: false,
            marked: false,
            by_item: false,
        };
        logs.buffer.add_as_native_code(saved);
    });
}

/// How many saves the buffer has room for.
fn room() -> usize {
    LOGS.with_borrow(|logs| logs.buffer.room())
}

/// A million writes of one word, each saved as native code saves it, without asking the runtime,
/// hold a few entries at most: each time the buffer's full, the runtime drops the saves of places
/// saved already before it makes room. A crash puts back the first, and a normal end keeps the last.
#[test]
fn a_million_writes_of_one_word_saved_without_the_runtime_hold_a_few_entries() {
    begin_run();
    let cell = locked(&["one"]);
    for turn in 0..1_000_000 {
        save_as_native_code(cell, COUNT_AT, turn + 10);
        set_word(cell, COUNT_AT, turn);
    }
    assert!(room() <= 16, "room for {} saves", room());
    assert!(saved_entries() <= 16);
    crash(cell);
    assert_eq!(word(cell, COUNT_AT), 10, "the first save's");
    shared_leave(cell, 1);
    shared_enter(cell, 1);
    begin(cell);
    for turn in 0..1_000_000 {
        save_as_native_code(cell, COUNT_AT, word(cell, COUNT_AT));
        set_word(cell, COUNT_AT, turn);
    }
    kept();
    assert_eq!(saved_entries(), 0);
    assert_eq!(word(cell, COUNT_AT), 999_999);
    shared_leave(cell, 1);
    finish();
}

/// Saves native code added are taken in before the runtime adds one: a place it saved is saved
/// once, whichever saved it first, and a crash puts back the first.
#[test]
fn saves_native_code_added_count_as_the_runtime_s_own() {
    begin_run();
    let cell = locked(&["one"]);
    save_as_native_code(cell, COUNT_AT, 3);
    save_as_native_code(cell, COUNT_AT, 4);
    save(cell, i64::try_from(COUNT_AT).unwrap(), 5, 0);
    assert_eq!(saved_entries(), 1);
    save_as_native_code(cell, COUNT_AT, 6);
    assert_eq!(saved_entries(), 2, "not yet taken in");
    set_word(cell, COUNT_AT, 8);
    crash(cell);
    assert_eq!(word(cell, COUNT_AT), 3);
    assert_eq!(saved_entries(), 0);
    shared_leave(cell, 1);
    finish();
}

/// With locks taken together, plain saves of the outer's cell made inside the inner are the outer's:
/// the inner's crash leaves them, and the outer's puts them back. The inner's own go with it.
#[test]
fn an_inner_crash_leaves_the_outer_cell_s_plain_saves_for_the_outer() {
    begin_run();
    let outer = locked(&["outer"]);
    let inner = locked(&["inner"]);
    save_as_native_code(outer, COUNT_AT, 3);
    set_word(outer, COUNT_AT, 5);
    save_as_native_code(inner, COUNT_AT, 3);
    set_word(inner, COUNT_AT, 6);
    save_as_native_code(inner, COUNT_AT, 6);
    crash(inner);
    assert_eq!(word(inner, COUNT_AT), 3);
    shared_leave(inner, 1);
    assert_eq!(word(outer, COUNT_AT), 5, "the inner's crash leaves the outer's cell");
    assert_eq!(saved_entries(), 1);
    crash(outer);
    assert_eq!(word(outer, COUNT_AT), 3);
    shared_leave(outer, 1);
    assert_eq!(saved_entries(), 0);
    finish();
}

/// A buffer grown past what's kept is let go of once no lock is held, so one big lock doesn't keep
/// its memory from the rest of the program.
#[test]
fn a_buffer_grown_big_goes_when_the_last_lock_ends() {
    begin_run();
    let cell = locked(&[]);
    for at in 0..(KEPT_ROOM + 10) {
        save(cell, i64::try_from(at).unwrap(), 0, 0);
    }
    assert!(room() > KEPT_ROOM);
    kept();
    assert_eq!(room(), 0);
    shared_leave(cell, 1);
    finish();
}

/// A `shared` value holding a `Log` of these lines and the number 3, locked to write, its log not
/// yet begun. Gives back its cell.
fn taken(items: &[&str]) -> i64 {
    let held = list(items);
    let cell = heap::alloc(LOG);
    set_word(cell, LINES_AT, held);
    set_word(cell, COUNT_AT, 3);
    shared_new(cell);
    shared_enter(cell, 1);
    cell
}

/// A word of the record, as native code reads it, without the runtime looking.
fn record_word(at: usize) -> usize {
    LOGS.with_borrow(|logs| logs.buffer.read(at))
}

/// What native code does as a lock's taken: once the record's made, with no lock held, it notes the
/// lock's cell as pending, with no call; otherwise it calls `begin`. Says whether it was pending.
fn begin_as_native_code(cell: i64) -> bool {
    let pending = LOGS.with_borrow_mut(|logs| {
        let buffer = &mut logs.buffer;
        if buffer.record.is_empty() || buffer.read(LOCKS) != 0 {
            return false;
        }
        buffer.put(PENDING, usize::try_from(cell).unwrap());
        buffer.put(LOCKS, 1);
        buffer.put(START, buffer.read(LEN));
        true
    });
    if !pending {
        begin(cell);
    }
    pending
}

/// What native code does at a lock's normal end: one still pending empties the buffer back to its
/// start, with no call; any other calls `kept`. Says whether it was pending.
fn kept_as_native_code() -> bool {
    let pending = LOGS.with_borrow_mut(|logs| {
        let buffer = &mut logs.buffer;
        if buffer.read(PENDING) == 0 {
            return false;
        }
        buffer.put(PENDING, 0);
        buffer.put(LOCKS, 0);
        buffer.put(LEN, buffer.read(START));
        true
    });
    if !pending {
        kept();
    }
    pending
}

/// A first lock on `cell`, which asks the runtime, as the record's not made yet, and saves a word,
/// so the buffer has room for native code's saves after it.
fn first_lock(cell: i64) {
    assert!(!begin_as_native_code(cell), "the first lock makes the record");
    save_as_native_code(cell, COUNT_AT, word(cell, COUNT_AT));
    assert!(!kept_as_native_code());
    assert!(room() > 0);
}

/// How many logs there are, without making a pending one's.
fn logs_made() -> usize {
    LOGS.with_borrow(|logs| logs.held.len())
}

/// A lock taken with no other held begins and ends with no log once the record's made, keeping what
/// it saved as plain words: the first lock asks the runtime, for the record.
#[test]
fn a_lock_alone_that_saves_only_plain_words_begins_and_ends_with_no_log() {
    begin_run();
    let cell = taken(&["one"]);
    first_lock(cell);
    for turn in 0..3 {
        assert!(begin_as_native_code(cell));
        assert_eq!((logs_made(), record_word(LOCKS)), (0, 1));
        save_as_native_code(cell, COUNT_AT, word(cell, COUNT_AT));
        set_word(cell, COUNT_AT, 10 + turn);
        assert!(kept_as_native_code());
        assert_eq!((record_word(LEN), record_word(LOCKS), record_word(PENDING)), (0, 0, 0));
    }
    assert_eq!(word(cell, COUNT_AT), 12);
    assert_eq!((held(), saved_entries()), (0, 0));
    shared_leave(cell, 1);
    finish();
}

/// A pending lock's log is made the first time the runtime's asked to save, from where its plain
/// saves start, so a crash puts back what native code saved before then and what the runtime saved
/// after.
#[test]
fn a_pending_lock_s_log_is_made_when_the_runtime_saves_and_a_crash_puts_back_both() {
    begin_run();
    let cell = taken(&["one", "two"]);
    first_lock(cell);
    assert!(begin_as_native_code(cell));
    save_as_native_code(cell, COUNT_AT, 3);
    set_word(cell, COUNT_AT, 9);
    assert_eq!(logs_made(), 0);
    let inside = word(cell, LINES_AT);
    save(cell, i64::try_from(LINES_AT).unwrap(), inside, 1);
    set_word(cell, LINES_AT, list(&["changed"]));
    assert_eq!((logs_made(), record_word(PENDING), record_word(LOCKS)), (1, 0, 1));
    crash(cell);
    assert_eq!(word(cell, COUNT_AT), 3);
    assert_eq!(word(cell, LINES_AT), inside);
    assert_eq!(lines(inside), ["one", "two"]);
    assert_eq!((held(), record_word(LOCKS), saved_entries()), (0, 0, 0));
    shared_leave(cell, 1);
    finish();
}

/// A crash in a lock still pending puts back its plain saves: the runtime makes its log to undo it.
#[test]
fn a_crash_in_a_pending_lock_puts_back_its_plain_saves() {
    begin_run();
    let cell = taken(&["one"]);
    first_lock(cell);
    assert!(begin_as_native_code(cell));
    save_as_native_code(cell, COUNT_AT, 3);
    set_word(cell, COUNT_AT, 7);
    save_as_native_code(cell, COUNT_AT, 7);
    set_word(cell, COUNT_AT, 8);
    crash(cell);
    assert_eq!(word(cell, COUNT_AT), 3);
    assert_eq!((held(), record_word(LOCKS), record_word(PENDING), saved_entries()), (0, 0, 0, 0));
    shared_leave(cell, 1);
    finish();
}

/// A lock taken inside a pending one makes the pending one's log first, as its table comes in, with
/// the tables behind as they were when it began; the inner one asks the runtime. What the inner
/// changed of the outer's still goes back on the outer's crash.
#[test]
fn a_lock_inside_a_pending_one_makes_its_log_first_with_the_tables_it_began_with() {
    begin_run();
    let outer = taken(&["outer"]);
    first_lock(outer);
    assert!(begin_as_native_code(outer));
    let outer_list = word(outer, LINES_AT);
    let line = word(outer_list, LIST_HEADER_WORDS);
    save_as_native_code(outer, COUNT_AT, 3);
    set_word(outer, COUNT_AT, 9);
    let inner = taken(&["inner"]);
    assert_eq!(logs_made(), 1, "the inner's table coming in made the outer's log");
    assert!(!begin_as_native_code(inner));
    assert_eq!(read_logs(|held| held.iter().map(|log| log.depth).collect::<Vec<_>>()), [1, 2]);
    save(outer_list, i64::try_from(LIST_HEADER_WORDS).unwrap(), line, 1);
    set_word(outer_list, LIST_HEADER_WORDS, string("changed"));
    save_as_native_code(inner, COUNT_AT, 3);
    set_word(inner, COUNT_AT, 5);
    assert!(!kept_as_native_code());
    shared_leave(inner, 1);
    assert_eq!(held(), 1);
    assert_eq!(count_of(line), 1, "the outer's log holds what was taken out of it");
    crash(outer);
    assert_eq!(word(outer, COUNT_AT), 3);
    assert_eq!(word(outer_list, LIST_HEADER_WORDS), line);
    assert_eq!(lines(outer_list), ["outer"]);
    assert_eq!((held(), record_word(LOCKS)), (0, 0));
    shared_leave(outer, 1);
    shared_enter(inner, 1);
    assert_eq!(word(inner, COUNT_AT), 5, "the inner kept its change");
    shared_leave(inner, 1);
    finish();
}

/// The shape number of a `Shelf`: a list of lists of strings, and a list of `Bag`s held in place.
const SHELF: i64 = 0;
/// Where a `Shelf`'s two lists are among its words.
const ROWS_AT: usize = heap::HEADER_WORDS;
const BAGS_AT: usize = heap::HEADER_WORDS + 1;
/// The kinds of item: a string, a list of strings, and a `Bag` of one list of strings, in place.
const ROW: i64 = 1;
const BAG: i64 = 2;

fn shelf_tables() -> Tables {
    let strings = Rc::new(Element::Str);
    let row = Rc::new(Element::List(Rc::clone(&strings)));
    let bag = Element::Struct {
        name: Rc::from("Bag"),
        fields: vec![(Rc::from("items"), Element::List(Rc::clone(&strings)))],
    };
    let shelf = Element::Struct {
        name: Rc::from("Shelf"),
        fields: vec![
            (Rc::from("rows"), Element::List(Rc::clone(&row))),
            (Rc::from("bags"), Element::List(Rc::new(bag.clone()))),
        ],
    };
    Tables {
        shapes: vec![Rc::new(shelf), Rc::new(Element::Str)],
        elements: vec![strings, row, Rc::new(bag)],
        str_shape: 1,
        ..Tables::default()
    }
}

/// A `shared` `Shelf` whose rows and bags each hold one list with one line, locked to write with its
/// log begun and the list at `field` saved, as `lock shelf as ref held` leaves it before a method
/// changes `held.rows` or `held.bags`. Gives back the cell.
fn locked_shelf(field: usize) -> i64 {
    let cell = heap::alloc(SHELF);
    for (at, kind) in [(ROWS_AT, ROW), (BAGS_AT, BAG)] {
        let outer = lists::new(kind, 1);
        set_word(outer, LIST_HEADER_WORDS, list(&["first, a line long enough for a block of its own"]));
        set_word(cell, at, outer);
    }
    shared_new(cell);
    shared_enter(cell, 1);
    begin(cell);
    let inside = word(cell, field);
    save_ref(cell, i64::try_from(field).unwrap(), inside);
    cell
}

/// What native code does for `held.rows[0].append(text)`, or `held.bags[0].items.append(text)`:
/// asks for the item to change, appends to the list it holds, and puts that list back in the item
/// where it now is.
fn appended_through_the_item(outer: i64, text: &str) {
    assert_eq!(lists::item_unshare(outer, 0), outer, "the marked list stays where it is");
    let inner = word(outer, LIST_HEADER_WORDS);
    let grown = lists::grow(inner);
    let length = usize::try_from(word(grown, 2)).unwrap();
    set_word(grown, LIST_HEADER_WORDS + length - 1, string(text));
    set_word(outer, LIST_HEADER_WORDS, grown);
}

/// What native code does on a crash, for a `Shelf`'s two fields.
fn crash_shelf(cell: i64) {
    undo();
    for at in [ROWS_AT, BAGS_AT] {
        let back = undo_word(i64::try_from(at).unwrap(), word(cell, at), 1);
        set_word(cell, at, back);
    }
}

fn copies() -> u64 {
    with(|state| state.heap.counts.copies)
}

/// Every block behind the lock is counted as many times as it's held, by the audit's own count.
fn audited(cell: i64) {
    let cell = address(cell);
    let found = with(|state| heap::audit::audit(state, &[cell]));
    let wrong: Vec<_> = found.wrong.into_iter().filter(|finding| finding.address != cell).collect();
    assert_eq!(wrong, Vec::new());
}

/// Under a lock, a list held in a list's item, a row or a `Bag`'s list, is appended to 300 times,
/// as a `ref self` method does: nothing's copied, the item's set is noted once, and a normal end
/// keeps every line, with nothing else left. The item's set once noted every time, with an owner
/// of the list it held, so every append copied the whole list and the log kept each copy (#1895).
#[test]
fn appends_to_a_list_in_a_lists_item_under_a_lock_copy_nothing_and_stay_at_an_end() {
    for field in [ROWS_AT, BAGS_AT] {
        start(shelf_tables(), crate::Inputs::default());
        let cell = locked_shelf(field);
        let (before, copied) = (blocks(), copies());
        let outer = word(cell, field);
        let first = word(outer, LIST_HEADER_WORDS);
        let texts: Vec<String> =
            (0..300).map(|turn| format!("by a method in turn {turn}, long enough")).collect();
        for text in &texts {
            appended_through_the_item(outer, text);
        }
        assert_eq!(copies(), copied, "field {field}: an append copied");
        assert_ne!(word(outer, LIST_HEADER_WORDS), first, "field {field}: the list never had to move");
        kept();
        let inner = word(word(cell, field), LIST_HEADER_WORDS);
        let mut wanted = vec!["first, a line long enough for a block of its own".to_owned()];
        wanted.extend(texts.iter().cloned());
        assert_eq!(lines(inner), wanted, "field {field}");
        assert_eq!(count_of(inner), 1, "field {field}");
        assert_eq!(blocks(), before + 300, "field {field}: the 300 lines are all that's new");
        audited(cell);
        shared_leave(cell, 1);
        finish();
    }
}

/// The same 300 appends, then a crash: the item holds its list as it was before the lock, with its
/// one line, though the list moved to a bigger block under the lock, and everything the appends
/// made is let go of. The item's set holds the list's old place, which a crash would have written
/// back: it follows the list as it moves.
#[test]
fn appends_to_a_list_in_a_lists_item_under_a_lock_go_back_on_a_crash_though_it_moved() {
    for field in [ROWS_AT, BAGS_AT] {
        start(shelf_tables(), crate::Inputs::default());
        let cell = locked_shelf(field);
        let (before, copied) = (blocks(), copies());
        let outer = word(cell, field);
        let first = word(outer, LIST_HEADER_WORDS);
        for turn in 0..300 {
            appended_through_the_item(outer, &format!("by a method in turn {turn}, long enough"));
        }
        assert_ne!(word(outer, LIST_HEADER_WORDS), first, "field {field}: the list never had to move");
        crash_shelf(cell);
        let inner = word(word(cell, field), LIST_HEADER_WORDS);
        assert_eq!(lines(inner), ["first, a line long enough for a block of its own"], "field {field}");
        assert_eq!(count_of(inner), 1, "field {field}");
        assert_eq!(copies(), copied, "field {field}");
        assert_eq!(blocks(), before, "field {field}: what the appends made is let go of");
        audited(cell);
        shared_leave(cell, 1);
        finish();
    }
}

/// An item appended through, then replaced by another list: a crash puts back the item as it was
/// before the lock, and a normal end keeps the new list, the old one let go of. Each with nothing
/// left over.
#[test]
fn an_item_appended_through_then_replaced_goes_back_on_a_crash_and_stays_replaced_at_an_end() {
    for crashes in [true, false] {
        start(shelf_tables(), crate::Inputs::default());
        let cell = locked_shelf(ROWS_AT);
        let before = blocks();
        let outer = word(cell, ROWS_AT);
        appended_through_the_item(outer, "appended, a line long enough for a block of its own");
        // `held.rows[0] = ["new"]`: the item asked for, the new list written, the old let go of.
        assert_eq!(lists::item_unshare(outer, 0), outer);
        let old = word(outer, LIST_HEADER_WORDS);
        set_word(outer, LIST_HEADER_WORDS, list(&["new, a line long enough for a block of its own"]));
        with(|state| heap::release(state, address(old)));
        if crashes {
            crash_shelf(cell);
            let inner = word(word(cell, ROWS_AT), LIST_HEADER_WORDS);
            assert_eq!(lines(inner), ["first, a line long enough for a block of its own"]);
            assert_eq!(blocks(), before);
        } else {
            kept();
            let inner = word(word(cell, ROWS_AT), LIST_HEADER_WORDS);
            assert_eq!(lines(inner), ["new, a line long enough for a block of its own"]);
            // The first list went with its line and the one appended under the lock, and the new
            // list came with its line: as many as before, one made under the lock gone again.
            assert_eq!(blocks(), before);
        }
        audited(cell);
        shared_leave(cell, 1);
        finish();
    }
}

/// A `Shelf` locked as `locked_shelf` leaves one, with `rows` holding the lists `rows` gives, by
/// address: the same address twice is one list held by two items.
fn locked_rows(rows: &[i64]) -> i64 {
    let cell = heap::alloc(SHELF);
    let outer = lists::new(ROW, i64::try_from(rows.len()).unwrap());
    for (at, row) in rows.iter().enumerate() {
        set_word(outer, LIST_HEADER_WORDS + at, *row);
    }
    set_word(cell, ROWS_AT, outer);
    set_word(cell, BAGS_AT, lists::new(BAG, 0));
    shared_new(cell);
    shared_enter(cell, 1);
    begin(cell);
    let inside = word(cell, ROWS_AT);
    save_ref(cell, i64::try_from(ROWS_AT).unwrap(), inside);
    cell
}

/// `held.rows[at].append(text)`, as native code does it.
fn appended_at(outer: i64, at: usize, text: &str) {
    assert_eq!(lists::item_unshare(outer, i64::try_from(at).unwrap()), outer);
    let inner = word(outer, LIST_HEADER_WORDS + at);
    let grown = lists::grow(inner);
    let length = usize::try_from(word(grown, 2)).unwrap();
    set_word(grown, LIST_HEADER_WORDS + length - 1, string(text));
    set_word(outer, LIST_HEADER_WORDS + at, grown);
}

/// What each of a shelf's rows holds.
fn rows_of(cell: i64) -> Vec<Vec<String>> {
    let outer = word(cell, ROWS_AT);
    let count = usize::try_from(word(outer, 2)).unwrap();
    (0..count).map(|at| lines(word(outer, LIST_HEADER_WORDS + at))).collect()
}

/// Three buckets appended to in turn, 60 times each, as `buckets[k].append(v)` does: nothing's
/// copied, each item's set noted once though other items' sets come between, and a crash puts
/// every bucket back as it was, a normal end keeping every line, with every count right.
#[test]
fn buckets_appended_to_in_turn_under_a_lock_copy_nothing_and_count_right_either_way() {
    for crashes in [true, false] {
        start(shelf_tables(), crate::Inputs::default());
        let firsts = [list(&["zero"]), list(&["one"]), list(&["two"])];
        let cell = locked_rows(&firsts);
        let (before, copied) = (blocks(), copies());
        let outer = word(cell, ROWS_AT);
        for turn in 0..180 {
            appended_at(outer, turn % 3, &format!("turn {turn}, a line long enough for a block"));
        }
        assert_eq!(copies(), copied, "crashes: {crashes}");
        if crashes {
            crash_shelf(cell);
            assert_eq!(rows_of(cell), [["zero"], ["one"], ["two"]]);
            assert_eq!(blocks(), before);
        } else {
            kept();
            for (at, row) in rows_of(cell).iter().enumerate() {
                assert_eq!(row.len(), 61, "row {at}");
            }
            assert_eq!(blocks(), before + 180);
        }
        audited(cell);
        shared_leave(cell, 1);
        finish();
    }
}

/// One list held by two items, appended to through each. The first append copies it, as it has
/// two owners, and so does the second, as the first item's set holds an owner of it now. A crash
/// gives it back both its holders, with the count to match, and a normal end lets it go, each item
/// holding a copy of its own. A second set that named it without an owner once left it one short:
/// freed while an item still held it.
#[test]
fn one_list_in_two_items_appended_through_each_counts_right_either_way() {
    for crashes in [true, false] {
        start(shelf_tables(), crate::Inputs::default());
        let first = list(&["shared, a line long enough for a block of its own"]);
        with(|state| heap::retain(state, address(first)));
        let cell = locked_rows(&[first, first]);
        let before = blocks();
        let outer = word(cell, ROWS_AT);
        // Making the cell `shared` copied it, the one list still held by both items.
        let shared = word(outer, LIST_HEADER_WORDS);
        assert_eq!(word(outer, LIST_HEADER_WORDS + 1), shared);
        assert_eq!(count_of(shared), 2);
        appended_at(outer, 0, "through the first, a line long enough for a block");
        appended_at(outer, 1, "through the second, a line long enough for a block");
        if crashes {
            crash_shelf(cell);
            assert_eq!(word(outer, LIST_HEADER_WORDS), shared);
            assert_eq!(word(outer, LIST_HEADER_WORDS + 1), shared);
            assert_eq!(count_of(shared), 2, "both items hold it, and nothing else does");
            assert_eq!(blocks(), before);
        } else {
            kept();
            let rows = rows_of(cell);
            assert_eq!(rows[0].len(), 2);
            assert_eq!(rows[1].len(), 2);
            assert!(!exists(shared), "neither item holds it now");
        }
        audited(cell);
        shared_leave(cell, 1);
        finish();
    }
}

/// An item set, then an append to the outer list, then the item set again: nothing moved, so it's
/// noted once. Then an item put in at the front, which moves the first one up, and that one set
/// again at its new index: noted again, with an owner of the list it holds, as it's marked already.
/// A crash puts back the rows as they were, the one put in taken out, and a normal end keeps all of
/// it, with every count right either way.
#[test]
fn an_item_set_again_after_an_append_or_an_insert_counts_right_either_way() {
    for crashes in [true, false] {
        start(shelf_tables(), crate::Inputs::default());
        let cell = locked_rows(&[list(&["first, a line long enough for a block"])]);
        let before = blocks();
        let outer = word(cell, ROWS_AT);
        appended_at(outer, 0, "before the append, a line long enough for a block");
        // `held.rows.append(["added"])`, which moves no item.
        let grown = lists::grow(outer);
        assert_eq!(grown, outer, "the marked list grows where it is");
        set_word(outer, LIST_HEADER_WORDS + 1, list(&["added, a line long enough for a block"]));
        appended_at(outer, 0, "after the append, a line long enough for a block");
        // `held.rows.insert(0, ["in front"])`, which moves the first item up one.
        let inserted = lists::insert(outer, 0, 0, 0);
        assert_eq!(inserted, outer);
        set_word(outer, LIST_HEADER_WORDS, list(&["in front, a line long enough for a block"]));
        appended_at(outer, 1, "after the insert, a line long enough for a block");
        if crashes {
            crash_shelf(cell);
            assert_eq!(rows_of(cell), [["first, a line long enough for a block"]]);
            assert_eq!(blocks(), before);
        } else {
            kept();
            let rows = rows_of(cell);
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0], ["in front, a line long enough for a block"]);
            assert_eq!(rows[1].len(), 4);
            assert_eq!(rows[2], ["added, a line long enough for a block"]);
        }
        audited(cell);
        shared_leave(cell, 1);
        finish();
    }
}

/// The shape numbers of the boxed tests' `Holder` and `Bag`, which is a struct of its own here,
/// held by a list of boxed `Bag`s.
const HOLDER: i64 = 0;
const BAG_SHAPE: i64 = 1;
const BAGS_HELD_AT: usize = heap::HEADER_WORDS;
const ITEMS_AT: usize = heap::HEADER_WORDS;

fn boxed_tables() -> Tables {
    let strings = Rc::new(Element::Str);
    let bag = Element::Struct {
        name: Rc::from("Bag"),
        fields: vec![(Rc::from("items"), Element::List(Rc::clone(&strings)))],
    };
    let holder = Element::Struct {
        name: Rc::from("Holder"),
        fields: vec![(Rc::from("bags"), Element::List(Rc::new(Element::Boxed(1))))],
    };
    Tables {
        shapes: vec![Rc::new(holder), Rc::new(bag), Rc::new(Element::Str)],
        elements: vec![strings, Rc::new(Element::Boxed(1))],
        str_shape: 2,
        ..Tables::default()
    }
}

/// A list of boxed structs, each a block of its own holding a list, appended to through the first
/// 300 times: the item's set marks the struct's block and saves its field, which marks the list,
/// so nothing's copied. A crash puts the struct and its list back as they were, though the list
/// moved, and a normal end keeps every line, with every count right either way.
#[test]
fn a_list_in_a_boxed_struct_in_a_lists_item_is_appended_to_where_it_is_either_way() {
    for crashes in [true, false] {
        start(boxed_tables(), crate::Inputs::default());
        let bag = heap::alloc(BAG_SHAPE);
        set_word(bag, ITEMS_AT, list(&["first, a line long enough for a block"]));
        let bags = lists::new(1, 1);
        set_word(bags, LIST_HEADER_WORDS, bag);
        let cell = heap::alloc(HOLDER);
        set_word(cell, BAGS_HELD_AT, bags);
        shared_new(cell);
        shared_enter(cell, 1);
        begin(cell);
        let outer = word(cell, BAGS_HELD_AT);
        save_ref(cell, i64::try_from(BAGS_HELD_AT).unwrap(), outer);
        let (before, copied) = (blocks(), copies());
        let first_items = word(word(outer, LIST_HEADER_WORDS), ITEMS_AT);
        for turn in 0..300 {
            // `held.bags[0].items.append(text)`: the item asked for, its struct to change asked
            // for, the list appended to and put back in the struct where it now is.
            assert_eq!(lists::item_unshare(outer, 0), outer);
            let bag = word(outer, LIST_HEADER_WORDS);
            assert_eq!(to_change(bag), bag, "the struct's block was copied");
            let grown = lists::grow(word(bag, ITEMS_AT));
            let length = usize::try_from(word(grown, 2)).unwrap();
            set_word(grown, LIST_HEADER_WORDS + length - 1, string(&format!("turn {turn}, long enough")));
            set_word(bag, ITEMS_AT, grown);
        }
        assert_eq!(copies(), copied, "crashes: {crashes}");
        assert_ne!(word(word(outer, LIST_HEADER_WORDS), ITEMS_AT), first_items, "the list never moved");
        if crashes {
            undo();
            let back = undo_word(i64::try_from(BAGS_HELD_AT).unwrap(), word(cell, BAGS_HELD_AT), 1);
            set_word(cell, BAGS_HELD_AT, back);
            let items = word(word(word(cell, BAGS_HELD_AT), LIST_HEADER_WORDS), ITEMS_AT);
            assert_eq!(lines(items), ["first, a line long enough for a block"]);
            assert_eq!(blocks(), before);
        } else {
            kept();
            let items = word(word(word(cell, BAGS_HELD_AT), LIST_HEADER_WORDS), ITEMS_AT);
            assert_eq!(lines(items).len(), 301);
            assert_eq!(blocks(), before + 300);
        }
        audited(cell);
        shared_leave(cell, 1);
        finish();
    }
}
