//! The layout of the runtime's blocks, for code generators that read them directly, and the
//! rules for doing that. Offsets are in bytes from a block's address.
//!
//! - **Owner counts aren't atomic.** A program runs on one thread.
//! - **Retaining** adds one to the count at [`COUNT`], and **releasing** subtracts one. Both can
//!   be done inline, without asking the runtime.
//! - **`fennec_free` is called only when a release reaches zero.** It releases what the block
//!   holds, then lets the block go.
//! - **A block counted [`super::heap::IMMORTAL`] is safe to do this to**: it's so high that no
//!   program gets it near zero, so it's never freed.
//! - **An address of zero holds nothing.** Check for it before retaining or releasing wherever
//!   a value can be missing.

/// A block's owner count.
pub const COUNT: i32 = 0;
/// A block's shape number.
pub const SHAPE: i32 = 8;
/// Where a boxed value's fields start.
pub const FIELDS: i32 = 16;

/// How many items a list has.
pub const LIST_LENGTH: i32 = 16;
/// How many items a list has room for.
pub const LIST_ROOM: i32 = 24;
/// Where a list's items start.
pub const LIST_ITEMS: i32 = 32;

/// A string's length in bytes.
pub const TEXT_BYTES: i32 = 16;
/// A string's length in characters.
pub const TEXT_CHARS: i32 = 24;
/// How many bytes a string has room for.
pub const TEXT_ROOM: i32 = 32;
/// Where a string's UTF-8 starts.
pub const TEXT: i32 = 40;

/// A generator's block, whose words come after its count and shape like a boxed value's. Every
/// generator's start the same way, so a loop can read one without knowing which it is. A
/// `Layout`'s field offsets count from [`FIELDS`], so add that to read one.
///
/// This is the address of the function that resumes it. Only generated code calls that, so
/// each backend can use its own calling convention.
pub const GEN_RESUME: i32 = 16;
/// Where it goes on from: 0 to start, positive from a `yield`, -1 finished, -2 running, -3 failed.
pub const GEN_PC: i32 = 24;
/// Whether anything has started using it.
pub const GEN_STARTED: i32 = 32;
/// What it failed with.
pub const GEN_FAILURE: i32 = 40;
/// The item it last gave.
pub const GEN_ITEM: i32 = 48;

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use fennec_syntax::Module;

    use super::super::{Tables, finish, heap, lists, regex, start, strings, with};
    use super::{FIELDS, LIST_ITEMS, LIST_LENGTH, LIST_ROOM, TEXT, TEXT_BYTES, TEXT_CHARS, TEXT_ROOM};
    use crate::packed::Element;

    /// Reads a word of a block.
    fn word(address: i64, offset: i32) -> Option<u64> {
        with(|state| {
            let block = state.heap.block(u64::try_from(address).ok()?)?;
            block.get(usize::try_from(offset / 8).ok()?).copied()
        })
    }

    fn text_of(address: i64) -> String {
        with(|state| {
            u64::try_from(address)
                .ok()
                .and_then(|at| state.heap.block(at))
                .map_or_else(String::new, heap::block_text)
        })
    }

    #[test]
    fn a_list_with_room_is_empty_and_has_it() {
        let tables = Tables { elements: vec![Rc::new(Element::Int)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let list = lists::with_room(0, 100);
        assert_eq!(word(list, LIST_LENGTH), Some(0));
        assert_eq!(word(list, LIST_ROOM), Some(100));
        finish();
    }

    #[test]
    fn a_string_with_room_is_filled_by_the_caller_and_grows_in_place() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let text = strings::with_room(13);
        assert_eq!(word(text, 0), Some(1));
        assert_eq!(word(text, TEXT_ROOM), Some(16));
        assert_eq!(text_of(text), "");
        set_word(text, TEXT, u64::from_le_bytes(*b"hello\0\0\0"));
        set_word(text, TEXT_BYTES, 5);
        set_word(text, TEXT_CHARS, 5);
        let more = with(|state| heap::text_address(state, " you"));
        assert_eq!(strings::append(text, more), text);
        assert_eq!(text_of(text), "hello you");
        assert_eq!(word(strings::with_room(-1), TEXT_ROOM), Some(8));
        finish();
    }

    #[test]
    fn numbers_format_without_a_temporary() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let (two, comma) = with(|state| (heap::text_address(state, ".2f"), heap::text_address(state, ",")));
        assert_eq!(text_of(strings::float_spec_text(1.5, two, 0, 0)), "1.50");
        assert_eq!(text_of(strings::int_spec_text(1000, comma, 0, 0)), "1,000");
        finish();
    }

    /// These restate how the runtime lays out blocks, for code generators. They can't drift.
    #[test]
    fn the_offsets_match_the_runtime() {
        let words = |offset: i32| usize::try_from(offset / 8).unwrap_or(0);
        assert_eq!(words(FIELDS), heap::HEADER_WORDS);
        assert_eq!(words(LIST_ITEMS), lists::LIST_HEADER_WORDS);
        assert_eq!(words(TEXT), heap::TEXT_HEADER_WORDS);
    }

    fn set_word(address: i64, offset: i32, value: u64) {
        with(|state| {
            let at = u64::try_from(address).unwrap_or(0);
            let word = usize::try_from(offset / 8).unwrap_or(0);
            if let Some(slot) = state.heap.block_mut(at).and_then(|block| block.get_mut(word)) {
                *slot = value;
            }
        });
    }

    thread_local! {
        static CLOSED: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
    }

    /// Closes a generator, calling back into the runtime as program code would.
    extern "C" fn close(generator: i64) {
        assert!(word(generator, super::GEN_PC).is_some_and(|pc| pc > 0 && pc < 10));
        let text = strings::int_text(5_000);
        heap::free(text);
        CLOSED.set(CLOSED.get() + 1);
    }

    fn generator_shape() -> Element {
        let int = || (Rc::from("word"), Element::Int);
        Element::Struct { name: Rc::from("gen"), fields: vec![int(), int(), int(), int(), int()] }
    }

    #[test]
    fn a_generator_stopped_at_a_yield_is_closed_when_it_is_freed() {
        let shapes = vec![Rc::new(generator_shape()), Rc::new(Element::Str)];
        let tables = Tables { shapes, str_shape: 1, ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        heap::set_abandon(0, close);
        CLOSED.set(0);
        let not_started = heap::alloc(0);
        heap::free(not_started);
        assert_eq!(CLOSED.get(), 0);
        let stopped = heap::alloc(0);
        set_word(stopped, super::GEN_PC, 3);
        heap::free(stopped);
        assert_eq!(CLOSED.get(), 1);
        assert_eq!(word(stopped, 0), None);
        finish();
    }

    #[test]
    fn a_generator_inside_a_list_inside_a_struct_is_closed_too() {
        let items = Rc::new(Element::Boxed(0));
        let holder = Element::Struct {
            name: Rc::from("holder"),
            fields: vec![(Rc::from("items"), Element::List(Rc::clone(&items)))],
        };
        let shapes = vec![Rc::new(generator_shape()), Rc::new(holder), Rc::new(Element::Str)];
        let tables = Tables { shapes, str_shape: 2, elements: vec![items], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        heap::set_abandon(0, close);
        CLOSED.set(0);
        let generator = heap::alloc(0);
        set_word(generator, super::GEN_PC, 1);
        let list = lists::new(0, 1);
        set_word(list, LIST_ITEMS, u64::try_from(generator).unwrap());
        let holder = heap::alloc(1);
        set_word(holder, FIELDS, u64::try_from(list).unwrap());
        heap::free(holder);
        assert_eq!(CLOSED.get(), 1);
        assert_eq!(word(generator, 0), None);
        finish();
    }

    /// Puts items in reverse order, calling back into the runtime while it does.
    extern "C" fn backward(a: i64, b: i64) -> i64 {
        heap::free(strings::int_text(1000));
        i64::from(a < b) - i64::from(a > b)
    }

    #[test]
    fn a_comparator_orders_a_copy_of_a_list() {
        let tables = Tables {
            elements: vec![Rc::new(Element::Int)],
            shapes: vec![Rc::new(Element::Str)],
            ..Tables::default()
        };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let list = lists::new(0, 3);
        for (at, value) in [(0, 1), (1, 2), (2, 3)] {
            set_word(list, LIST_ITEMS + at * 8, value);
        }
        let sorted = lists::sorted_with(list, backward);
        let read = |list: i64| [0, 1, 2].map(|at| word(list, LIST_ITEMS + at * 8).unwrap());
        assert_eq!(read(sorted), [3, 2, 1]);
        assert_eq!(read(list), [1, 2, 3]);
        finish();
    }

    #[test]
    fn regexes_work_by_address() {
        let tables = Tables {
            elements: vec![Rc::new(Element::Int), Rc::new(Element::Str)],
            shapes: vec![Rc::new(Element::Str)],
            ..Tables::default()
        };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let string = |text: &str| with(|state| heap::text_address(state, text));
        let (pattern, whole) = (string("é(b)|(c)"), string("aéb c"));
        assert_eq!(regex::is_valid(pattern), 1);
        assert_eq!(regex::is_valid(string("(")), 0);
        assert_eq!(
            (regex::groups(pattern), regex::groups(string("b")), regex::groups(string("("))),
            (2, 0, -1)
        );
        let places = regex::find_all(pattern, whole, 0, 0, 0);
        let minus = u64::from_ne_bytes((-1_i64).to_ne_bytes());
        let read = |at: i32| word(places, LIST_ITEMS + at * 8).unwrap();
        assert_eq!(word(places, LIST_LENGTH), Some(12));
        assert_eq!([read(0), read(1), read(2), read(3), read(4), read(5)], [1, 3, 2, 3, minus, minus]);
        assert_eq!([read(6), read(7), read(8), read(9), read(10), read(11)], [4, 5, minus, minus, 4, 5]);
        let swapped = regex::replace(pattern, whole, string("<\\1>"), 0, 0);
        assert_eq!(text_of(swapped), "a<b> <>");
        let parts = regex::split(string(" "), whole, 1, 0, 0);
        assert_eq!(word(parts, LIST_LENGTH), Some(2));
        let first = i64::try_from(word(parts, LIST_ITEMS).unwrap()).unwrap();
        assert_eq!(text_of(first), "aéb");
        let anchored = regex::find(string("b"), whole, 1, 0, 0, 0);
        assert_eq!(word(anchored, LIST_LENGTH), Some(0));
        let anywhere = regex::find(string("b"), whole, 0, 0, 0, 0);
        assert_eq!(word(anywhere, LIST_LENGTH), Some(2));
        finish();
    }

    #[test]
    fn stopping_fails_with_the_text_at_the_span() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let message = with(|state| heap::text_address(state, "assertion failed"));
        super::super::checks::stop(message, 3, 7);
        let (_, failure, _) = finish();
        let failure = failure.unwrap();
        assert_eq!(failure.message, "assertion failed");
        assert_eq!((failure.span.start, failure.span.end), (3, 7));
    }

    #[test]
    fn the_counts_line_says_what_was_copied() {
        let counts = heap::Counts {
            copies: 2,
            copied_bytes: 48,
            largest_copy: 32,
            allocations: 5,
            allocated_bytes: 640,
        };
        assert_eq!(
            super::super::aot::counts_line(&counts),
            "copies 2 copied_bytes 48 largest_copy 32 allocations 5 allocated_bytes 640"
        );
    }

    #[test]
    fn an_optional_block_reads_and_writes_as_one_word() {
        let maybe = Element::Maybe(Rc::new(Element::Str));
        let holder = Element::Struct { name: Rc::from("h"), fields: vec![(Rc::from("name"), maybe.clone())] };
        let tables = Tables {
            shapes: vec![Rc::new(holder), Rc::new(Element::Str)],
            str_shape: 1,
            ..Tables::default()
        };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        with(|state| {
            let mut words = Vec::new();
            heap::write(state, &maybe, &crate::value::Value::None, &mut words).unwrap();
            assert_eq!(words, [0]);
            assert_eq!(heap::read_word(state, &maybe, 0), crate::value::Value::None);
            let mut words = Vec::new();
            heap::write(state, &maybe, &crate::value::Value::str("hi"), &mut words).unwrap();
            let [address] = words.as_slice() else { panic!("one word") };
            assert_ne!(*address, 0);
            assert_eq!(heap::read_word(state, &maybe, *address), crate::value::Value::str("hi"));
        });
        let empty = heap::alloc(0);
        heap::free(empty);
        let named = heap::alloc(0);
        let text = with(|state| heap::text_address(state, "x"));
        set_word(named, FIELDS, u64::try_from(text).unwrap());
        heap::free(named);
        assert_eq!(word(text, 0), None);
        finish();
    }

    #[test]
    fn a_dict_is_read_and_changed_by_a_slice_of_text_without_making_its_key() {
        let shapes = vec![Rc::new(Element::Str)];
        let tables = Tables { shapes, str_shape: 0, ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let shape =
            super::super::dicts::shape(0, super::super::dicts::Word::Str, super::super::dicts::Word::Int)
                .unwrap();
        let mut dict = super::super::dicts::new(i64::try_from(shape).unwrap(), 4);
        let string = |text: &str| with(|state| heap::text_address(state, text));
        let (dna, accented) = (string("ggtacgt"), string("éaéb"));
        // A key added the usual way is found by a slice, so both hash it the same.
        dict = super::super::dicts::set(dict, string("ta"), 7);
        let found = super::super::dicts::find_slice(dict, dna, 2, 4);
        assert_ne!(found, 0);
        let before = with(|state| state.heap.counts.allocations);
        dict = super::super::dicts::add_slice(dict, dna, 2, 4, 8);
        assert_eq!(with(|state| state.heap.counts.allocations), before);
        dict = super::super::dicts::add_slice(dict, dna, -3, i64::MIN, 1);
        dict = super::super::dicts::add_slice(dict, accented, 2, 4, 2);
        let read = |dict: i64, text: i64, from: i64, to: i64| {
            let found = super::super::dicts::find_slice(dict, text, from, to);
            (found != 0).then(|| {
                with(|state| {
                    let base = state.heap.block(u64::try_from(dict).unwrap()).unwrap().as_ptr().addr();
                    let at = (usize::try_from(found).unwrap() - base) / 8;
                    state.heap.block(u64::try_from(dict).unwrap()).unwrap()[at]
                })
            })
        };
        assert_eq!(read(dict, dna, 2, 4), Some(8));
        assert_eq!(read(dict, dna, 4, 7), Some(1));
        assert_eq!(read(dict, accented, 2, 4), Some(2));
        assert_eq!(read(dict, string("éb"), 0, 2), Some(2));
        assert_eq!(read(dict, dna, 0, 2), None);
        assert_eq!(word(dict, 16), Some(3));
        finish();
    }

    #[test]
    fn indexing_text_reads_only_what_it_needs() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let string = |text: &str| with(|state| heap::text_address(state, text));
        let (plain, accented) = (string("abc"), string("aéb€c"));
        let at = |text, at| text_of(strings::index(text, at, 0, 0));
        assert_eq!([at(plain, 0), at(plain, 2), at(plain, -1)], ["a", "c", "c"]);
        assert_eq!(
            [at(accented, 1), at(accented, 2), at(accented, 3), at(accented, -1)],
            ["é", "b", "€", "c"]
        );
        assert_eq!(strings::index(plain, 3, 0, 0), 0);
        let (_, failure, _) = finish();
        assert!(failure.unwrap().message.contains("out of range"));
    }

    #[test]
    fn stripping_takes_off_what_str_trim_does() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let string = |text: &str| with(|state| heap::text_address(state, text));
        for text in ["  a b  ", "\t\n\x0b\x0cx\r ", "   ", "", "ok", " é ", "\x1fy\x1f", "a\u{a0}"] {
            for (which, expected) in [(0, text.trim()), (1, text.trim_start()), (2, text.trim_end())] {
                assert_eq!(text_of(strings::strip(string(text), which)), expected, "{text:?} {which}");
            }
        }
        finish();
    }

    /// Native code hashes string keys itself, slices of strings too, so this is fixed.
    #[test]
    fn a_string_key_s_hash_stays_what_native_code_works_out() {
        use super::super::dicts::hash_text;
        assert_eq!(hash_text(0, &[]), 0);
        let word = u64::from_le_bytes(*b"ggta\0\0\0\0");
        let mix: u64 = 0x9E37_79B9_7F4A_7C15;
        let h = (4_u64.wrapping_mul(mix).rotate_left(23) ^ word).wrapping_mul(mix);
        assert_eq!(hash_text(4, &[word]), (h ^ (h >> 29)) >> 1);
        assert_eq!(hash_text(4, &[word]), 0x6b45_6e2d_7b01_4de3);
    }

    #[test]
    fn popping_a_dict_key_hands_back_its_value() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let words = super::super::dicts::Word::Int;
        let shape = super::super::dicts::shape(0, words, words).unwrap();
        let mut dict = super::super::dicts::new(i64::try_from(shape).unwrap(), 4);
        dict = super::super::dicts::set(dict, 3, 30);
        dict = super::super::dicts::set(dict, 4, 40);
        dict = super::super::dicts::pop(dict, 3, 0, 0);
        assert_eq!(super::super::dicts::popped(), 30);
        assert_eq!(word(dict, 16), Some(1));
        super::super::dicts::pop(dict, 3, 0, 0);
        let (_, failure, _) = finish();
        assert_eq!(failure.unwrap().message, "the dict has no key 3");
    }

    #[test]
    fn a_struct_named_after_a_tuple_type_reads_and_writes_as_a_tuple() {
        let pair = Element::Struct {
            name: Rc::from("(int, str)"),
            fields: vec![(Rc::from("_0"), Element::Int), (Rc::from("_1"), Element::Str)],
        };
        let tables = Tables {
            shapes: vec![Rc::new(pair.clone()), Rc::new(Element::Str)],
            str_shape: 1,
            ..Tables::default()
        };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        with(|state| {
            let tuple =
                crate::value::Value::tuple(vec![crate::value::Value::Int(1), crate::value::Value::str("a")]);
            let mut words = Vec::new();
            heap::write(state, &pair, &tuple, &mut words).unwrap();
            let block = heap::write_block(state, 0, &tuple).unwrap();
            assert_eq!(heap::read_block(state, u64::try_from(block).unwrap()), tuple);
        });
        finish();
    }
}
