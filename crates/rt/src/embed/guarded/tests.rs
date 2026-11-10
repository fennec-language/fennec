use std::rc::Rc;
use std::sync::mpsc;

use super::*;
use crate::embed::{Tables, finish, heap, lists, start, threads, with};
use crate::packed::Element;

/// The shape number of a struct with a list of strings and a number, and another holding a handle.
const LOG: i64 = 0;
const OWNER: i64 = 1;
/// The kind of item of a list of strings.
const LINES: i64 = 0;

fn tables() -> Tables {
    let strings = Rc::new(Element::Str);
    let log = Element::Struct {
        name: Rc::from("Log"),
        fields: vec![
            (Rc::from("lines"), Element::List(Rc::clone(&strings))),
            (Rc::from("count"), Element::Int),
        ],
    };
    let owner =
        Element::Struct { name: Rc::from("Owner"), fields: vec![(Rc::from("log"), Element::Boxed(0))] };
    let str_shape = 2;
    Tables {
        shapes: vec![Rc::new(log), Rc::new(owner), Rc::new(Element::Str)],
        elements: vec![strings],
        str_shape,
        ..Tables::default()
    }
}

fn begin() {
    start(tables(), crate::Inputs::default());
}

/// A list with what `list` holds and `text`, in place of it.
fn appended(list: i64, text: &str) -> i64 {
    let mut held = lines(list);
    held.push(text.to_owned());
    let held: Vec<&str> = held.iter().map(String::as_str).collect();
    let grown = with(|state| lists::list_of_strings(state, LINES, &held));
    heap::free(list);
    grown
}

fn word(block: i64, at: usize) -> u64 {
    with(|state| state.heap.block(u64::try_from(block).unwrap()).map(|words| words[at])).unwrap_or(0)
}

fn set_word(block: i64, at: usize, value: i64) {
    with(|state| {
        if let Some(words) = state.heap.block_mut(u64::try_from(block).unwrap()) {
            words[at] = u64::try_from(value).unwrap();
        }
    });
}

/// What a list of strings holds, as this thread sees it.
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

const FIELDS: usize = heap::HEADER_WORDS;

/// A `Log` holding a list.
fn log_of(items: &[&str]) -> i64 {
    let list = with(|state| lists::list_of_strings(state, LINES, items));
    let log = heap::alloc(LOG);
    set_word(log, FIELDS, list);
    log
}

#[test]
fn a_new_shared_value_holds_copies_and_gives_up_what_it_was_given() {
    begin();
    let before = with(|state| state.heap.live());
    let log = log_of(&["one", "two"]);
    let given = i64::try_from(word(log, FIELDS)).unwrap();
    // Something else holds what it's given too, and keeps it. The value has its own.
    with(|state| heap::retain(state, u64::try_from(given).unwrap()));
    assert_eq!(shared_new(log), log);
    assert!(is_handle(u64::try_from(log).unwrap()));
    assert_eq!(lines(given), ["one", "two"]);
    assert_eq!(word(given, 0), 1, "it gave up its count");
    assert_eq!(shared_enter(log, 1), i64::try_from(GUARDED_OWNER).unwrap());
    let inside = i64::try_from(word(log, FIELDS)).unwrap();
    assert_ne!(inside, given);
    assert_eq!(lines(inside), ["one", "two"]);
    assert_eq!(shared_leave(log, 1), 0);
    assert_eq!(word(log, FIELDS), 0, "a thread not holding the lock can't see in");
    heap::free(given);
    with(|state| release_handle(state, u64::try_from(log).unwrap()));
    assert!(!is_handle(u64::try_from(log).unwrap()));
    assert_eq!(with(|state| state.heap.live()), before);
    finish();
}

#[test]
fn what_is_made_under_a_write_lock_stays_with_the_value_and_counts_as_this_threads() {
    begin();
    let log = log_of(&["one"]);
    shared_new(log);
    shared_enter(log, 1);
    let inside = i64::try_from(word(log, FIELDS)).unwrap();
    let grown = appended(inside, "two");
    set_word(log, FIELDS, grown);
    let shape = word(grown, 1);
    assert_eq!(shape >> OWNER_SHIFT, GUARDED_OWNER, "it's the table's");
    assert_eq!(word(grown, 0), 1);
    shared_leave(log, 1);
    assert_eq!(word(grown, 0), 0, "it left with the table");
    assert_eq!(with(|state| state.heap.made_so_far()), 0, "nothing's kept to hand over");
    shared_enter(log, 1);
    assert_eq!(lines(grown), ["one", "two"]);
    shared_leave(log, 1);
    finish();
}

#[test]
fn values_are_copied_at_the_edge_each_way() {
    begin();
    let log = log_of(&[]);
    shared_new(log);
    let mine = with(|state| lists::list_of_strings(state, LINES, &["mine"]));
    shared_enter(log, 1);
    let copy = shared_in(mine);
    assert_ne!(copy, mine);
    assert_eq!(word(copy, 1) >> OWNER_SHIFT, GUARDED_OWNER);
    assert_eq!(lines(copy), ["mine"]);
    // What's already there is the same value with one more owner.
    assert_eq!(shared_in(copy), copy);
    assert_eq!(word(copy, 0), 2);
    let out = shared_out(copy, 1);
    assert_ne!(out, copy);
    assert_eq!(word(out, 1) >> OWNER_SHIFT, 0, "it's this thread's");
    shared_leave(log, 1);
    assert_eq!(lines(out), ["mine"]);
    assert_eq!(word(out, 0), 1);
    finish();
}

#[test]
fn leaving_an_inner_lock_copies_to_the_table_of_the_lock_around_it() {
    begin();
    let (outer, inner) = (log_of(&[]), log_of(&["inner"]));
    shared_new(outer);
    shared_new(inner);
    shared_enter(outer, 1);
    shared_enter(inner, 1);
    let inside = i64::try_from(word(inner, FIELDS)).unwrap();
    let out = shared_out(inside, 1);
    shared_leave(inner, 1);
    assert_eq!(lines(out), ["inner"]);
    assert_eq!(word(out, 1) >> OWNER_SHIFT, GUARDED_OWNER, "it's the outer value's");
    shared_leave(outer, 1);
    assert_eq!(word(out, 0), 0, "it left with the outer value's table");
    finish();
}

#[test]
fn two_values_held_at_once_each_end_up_with_only_what_is_their_own() {
    begin();
    let (first, second) = (log_of(&["first"]), log_of(&["second"]));
    shared_new(first);
    shared_new(second);
    shared_enter(first, 1);
    shared_enter(second, 1);
    // The second's table is on top, and what's made goes there, whichever value it's for.
    let made = with(|state| lists::list_of_strings(state, LINES, &["made for the first"]));
    let old = i64::try_from(word(first, FIELDS)).unwrap();
    set_word(first, FIELDS, made);
    heap::free(old);
    shared_settle(first);
    assert_eq!(word(first, 0), 0, "the first's own block is still there to read");
    assert!(
        with(|state| state.heap.blocks.get(usize::try_from(second).unwrap()).is_some()),
        "the second's table is on top again"
    );
    shared_settle(second);
    shared_leave(second, 1);
    shared_leave(first, 1);
    shared_enter(first, 1);
    let inside = i64::try_from(word(first, FIELDS)).unwrap();
    assert_ne!(inside, made, "the first has its own");
    assert_eq!(lines(inside), ["made for the first"]);
    shared_leave(first, 1);
    shared_enter(second, 1);
    let inside = i64::try_from(word(second, FIELDS)).unwrap();
    assert_eq!(lines(inside), ["second"]);
    assert_eq!(word(made, 0), 0, "it isn't anywhere the second's table can be asked for");
    shared_leave(second, 1);
    finish();
}

#[test]
fn a_handle_inside_a_value_is_the_same_handle_with_one_more_count() {
    begin();
    let before = with(|state| state.heap.live());
    let log = log_of(&["one"]);
    shared_new(log);
    let address = u64::try_from(log).unwrap();
    let owner = heap::alloc(OWNER);
    set_word(owner, FIELDS, log);
    count_handle(address);
    assert_eq!(shared_new(owner), owner);
    // Letting go of the first handle leaves the one the other holds.
    with(|state| release_handle(state, address));
    assert!(is_handle(address));
    // And letting go of the value that holds it takes it too.
    with(|state| release_handle(state, u64::try_from(owner).unwrap()));
    assert!(!is_handle(address));
    assert_eq!(with(|state| state.heap.live()), before);
    finish();
}

/// A run that ends holding a `shared` value's write lock, as a rollback test does, forgets the value:
/// its table, and the block in it, went with the run's heap, so the next block put at that address
/// isn't taken for a handle to it. A value whose lock isn't held keeps its block, so stays. Asked of
/// what's kept of each, as another test can make a value at the address once it's free.
#[test]
fn a_run_that_ends_holding_a_lock_forgets_its_value() {
    let kept = |address: u64| super::tables(|tables| tables.get(&address).cloned());
    let still = |address: u64, slot: &Arc<Slot>| kept(address).is_some_and(|now| Arc::ptr_eq(&now, slot));
    begin();
    let locked = log_of(&["locked"]);
    shared_new(locked);
    let free = log_of(&["free"]);
    shared_new(free);
    shared_enter(locked, 1);
    let (locked, free) = (u64::try_from(locked).unwrap(), u64::try_from(free).unwrap());
    let (locked_slot, free_slot) = (kept(locked).unwrap(), kept(free).unwrap());
    finish();
    assert!(!still(locked, &locked_slot), "the locked value's block went with the run");
    assert!(still(free, &free_slot), "the other's is still in its table");
}

#[test]
fn another_thread_reads_and_changes_what_this_one_put_there() {
    begin();
    let log = log_of(&["from the first"]);
    shared_new(log);
    let (done, wait) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        threads::take_index();
        begin();
        for round in 0..100 {
            assert_eq!(shared_enter(log, 0), i64::from(threads::index()));
            let inside = i64::try_from(word(log, FIELDS)).unwrap();
            assert_eq!(lines(inside).len(), round + 1);
            // Reading out of the lock is a copy, in this thread's own table.
            let copy = shared_out(inside, 0);
            shared_leave(log, 0);
            assert_eq!(lines(copy).len(), round + 1);
            heap::free(copy);
            shared_enter(log, 1);
            let inside = i64::try_from(word(log, FIELDS)).unwrap();
            let grown = appended(inside, "from the second");
            set_word(log, FIELDS, grown);
            shared_leave(log, 1);
        }
        finish();
        done.send(()).unwrap();
    });
    thread.join().unwrap();
    wait.recv().unwrap();
    shared_enter(log, 1);
    let inside = i64::try_from(word(log, FIELDS)).unwrap();
    let seen = lines(inside);
    shared_leave(log, 1);
    assert_eq!(seen.len(), 101);
    assert_eq!(seen[0], "from the first");
    assert_eq!(seen[100], "from the second");
    finish();
}

/// Native code finds the word saying a run is recorded or replayed at 16 bytes past its failure
/// flag, after the lock it holds.
#[test]
fn the_replaying_word_is_where_native_code_reads_it() {
    assert_eq!(std::mem::offset_of!(crate::embed::Flags, lock_held), 8);
    assert_eq!(std::mem::offset_of!(crate::embed::Flags, replaying), 16);
}

/// Outside a recording or a replay, the word is 0, and the lock's hooks do nothing.
#[test]
fn outside_a_recording_the_lock_hooks_do_nothing() {
    let _ = crate::embed::failed_flag();
    assert_eq!(crate::embed::FLAGS.with(|flags| flags.replaying.get()), 0);
    replay_lock(0, 1);
    replay_locked(0, 1);
    assert!(!crate::embed::failed());
}
