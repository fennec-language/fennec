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
        let saved =
            Saved { block: address(block), word, old: old.cast_unsigned(), held: false, marked: false };
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
