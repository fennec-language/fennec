//! An error's notes, for native code: the word an error's layout keeps them in, `NOTES`, is 0
//! until a note is added, and a list of strings after. These take and give that word, so they
//! needn't know where an error's layout puts it, and an error with no notes costs nothing more.

use std::rc::Rc;

use fennec_syntax::{Span, Trace};

use super::heap::{read_block, release, text_address};
use super::{State, with};
use crate::value::Value;

/// What starts a note that's a place in the error's call chain rather than one the program added:
/// `raised` or `passed`, then the place's span. Never shown as a note or given by `notes()`.
const FRAME: &str = "\0frame ";

/// The notes a notes word holds that the program added: none for 0.
fn texts(state: &State, notes: i64) -> Vec<Rc<str>> {
    every_text(state, notes).into_iter().filter(|text| !text.starts_with(FRAME)).collect()
}

/// Every text a notes word holds, its call chain's places among them.
fn every_text(state: &State, notes: i64) -> Vec<Rc<str>> {
    let Ok(address) = u64::try_from(notes) else { return Vec::new() };
    if address == 0 {
        return Vec::new();
    }
    match read_block(state, address).unpacked() {
        Value::List(items) => items
            .iter()
            .filter_map(|item| if let Value::Str(text) = item { Some(Rc::clone(text)) } else { None })
            .collect(),
        _ => Vec::new(),
    }
}

/// `fennec_notes_add(notes, text)`: `error.add_note(text)`, on its notes word. It takes over the
/// word's owner, 0 when there are none yet, and the owner of `text`, a string's block, and gives
/// back the new word, which has the note last.
pub extern "C" fn add(notes: i64, text: i64) -> i64 {
    with(|state| {
        let mut all = every_text(state, notes);
        if let Ok(Value::Str(added)) = u64::try_from(text).map(|address| read_block(state, address)) {
            all.push(added);
        }
        let element = super::text::string_element(state);
        let list = Value::list(all.into_iter().map(Value::Str).collect());
        let made = super::lists::write_list(state, &element, &list).unwrap_or(notes);
        for address in [notes, text] {
            if let Ok(address) = u64::try_from(address)
                && address != 0
                && made != notes
            {
                release(state, address);
            }
        }
        made
    })
}

/// `fennec_notes_list(notes)`: `error.notes()`, a list of strings owned once by the caller: an
/// empty one for 0, the notes' own list with one more owner when they're all the program's, and
/// otherwise a list of those.
pub extern "C" fn list(notes: i64) -> i64 {
    with(|state| {
        if let Ok(address) = u64::try_from(notes)
            && address != 0
            && every_text(state, notes).len() == texts(state, notes).len()
        {
            super::heap::retain(state, address);
            return notes;
        }
        let element = super::text::string_element(state);
        let own = Value::list(texts(state, notes).into_iter().map(Value::Str).collect());
        super::lists::write_list(state, &element, &own).unwrap_or(0)
    })
}

/// `fennec_notes_frame(notes, raised, start, end)`: an error leaves a function at this span, by
/// `fail` if `raised` is 1 and by `?` if 0, which goes in its call chain. Takes over the word's
/// owner and gives back the new word, as `fennec_notes_add` does. The chain stays small however
/// far the error goes: the same place again is counted on the note before, and past twice
/// [`Trace::KEPT`] places, those after the first [`Trace::KEPT`] go into a count of the left out.
pub extern "C" fn frame(notes: i64, raised: i64, start: i64, end: i64) -> i64 {
    let how = if raised == 1 { "raised" } else { "passed" };
    let mut all = with(|state| every_text(state, notes));
    let place = format!("{FRAME}{how} {start} {end} ");
    match all.last_mut() {
        Some(last)
            if let Some(times) =
                last.strip_prefix(place.as_str()).and_then(|times| times.parse::<u64>().ok()) =>
        {
            *last = Rc::from(format!("{place}{}", times + 1));
        }
        _ => all.push(Rc::from(format!("{place}1"))),
    }
    let frames: Vec<usize> =
        all.iter().enumerate().filter(|(_, text)| text.starts_with(FRAME)).map(|(at, _)| at).collect();
    if frames.len() > 2 * Trace::KEPT + 1
        && let (Some(&count), Some(&gone)) = (frames.get(Trace::KEPT), frames.get(Trace::KEPT + 1))
    {
        let calls = |at: usize| {
            all.get(at)
                .and_then(|text| text.rsplit(' ').next())
                .and_then(|times| times.parse::<u64>().ok())
                .unwrap_or(1)
        };
        let counted = all.get(count).is_some_and(|text| text.starts_with(SKIPPED));
        // The first place past the kept ones becomes the count of those left out, and each after
        // it goes into that count.
        let left_out = if counted { calls(count) + calls(gone) } else { calls(count) };
        if counted {
            all.remove(gone);
        }
        if let Some(text) = all.get_mut(count) {
            *text = Rc::from(format!("{SKIPPED}{left_out}"));
        }
    }
    with(|state| {
        let element = super::text::string_element(state);
        let list = Value::list(all.into_iter().map(Value::Str).collect());
        let made = super::lists::write_list(state, &element, &list).unwrap_or(notes);
        if let Ok(address) = u64::try_from(notes)
            && address != 0
            && made != notes
        {
            release(state, address);
        }
        made
    })
}

/// What starts the note counting the places of a call chain left out of it.
const SKIPPED: &str = "\0frame skipped ";

/// `fennec_notes_traced(notes)`: the error in these notes has ended the program, which says so
/// at the place it was raised, or passed up into the program, with the rest of its call chain
/// under it. Where it went inside the standard library says nothing the program can act on, so
/// it's said where the program's own code first had it: the `?` it came into the program by.
pub extern "C" fn traced(notes: i64) {
    // Each place, and how often in a row, or `None` for those left out.
    let places: Vec<(Option<Span>, u64)> = with(|state| {
        every_text(state, notes)
            .iter()
            .filter_map(|text| {
                if let Some(skipped) = text.strip_prefix(SKIPPED) {
                    return Some((None, skipped.parse().ok()?));
                }
                let mut words = text.strip_prefix(FRAME)?.split(' ').skip(1);
                let start = words.next()?.parse().ok()?;
                let end = words.next()?.parse().ok()?;
                let times = words.next()?.parse().ok()?;
                Some((Some(Span { start, end }), times))
            })
            .collect()
    });
    let Some(own) = places.iter().position(|(place, _)| place.is_some_and(|place| !place.in_prelude()))
    else {
        return;
    };
    let Some((Some(first), times)) = places.get(own).copied() else { return };
    with(|state| {
        if let Some(failure) = &mut state.failure {
            failure.span = first;
        }
    });
    let mut trace = Trace::default();
    if times > 1 {
        trace.push(first, times - 1);
    }
    for (place, times) in places.iter().skip(own + 1) {
        match place {
            Some(place) => trace.push(*place, *times),
            None => trace.skip(*times),
        }
    }
    super::set_trace(trace);
}

/// `fennec_notes_shown(notes)`: what goes under a failure that reached `main`, a string owned
/// once: a line `note: ...` for each note, each starting with a line break, and nothing for 0.
/// A note's later lines are indented under its first.
pub extern "C" fn shown(notes: i64) -> i64 {
    super::made_with("fennec_notes_shown", |state| {
        let mut text = String::new();
        for note in texts(state, notes) {
            text.push_str("\nnote: ");
            text.push_str(&note.replace('\n', "\n  "));
        }
        text_address(state, &text)
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, start, with};
    use super::{add, list, shown};
    use crate::packed::Element;
    use crate::value::{Instance, Value};

    fn begin() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
    }

    fn text(address: i64) -> String {
        with(|state| match heap::read_block(state, u64::try_from(address).unwrap()) {
            Value::Str(text) => text.to_string(),
            other => panic!("not a string: {other:?}"),
        })
    }

    #[test]
    fn no_notes_is_a_zero_word_an_empty_list_and_nothing_shown() {
        begin();
        let none = list(0);
        with(|state| {
            let read = heap::read_block(state, u64::try_from(none).unwrap()).unpacked();
            assert!(matches!(read, Value::List(items) if items.is_empty()));
        });
        assert_eq!(text(shown(0)), "");
        finish();
    }

    /// `Lost { why: str }` as an error struct's layout has it, its notes last, as a list of
    /// strings.
    fn lost_layout() -> Rc<Element> {
        let fields = vec![
            (Rc::from("why"), Element::Str),
            (Rc::from(crate::packed::NOTES), Element::List(Rc::new(Element::Str))),
        ];
        Rc::new(Element::Struct { name: Rc::from("Lost"), fields })
    }

    fn lost(why: &str, notes: &[&str]) -> Value {
        Value::Instance(Rc::new(Instance {
            type_name: Rc::from("Lost"),
            case: None,
            fields: vec![(Rc::from("why"), Value::str(why))],
            is_error: true,
            cause: None,
            notes: (!notes.is_empty()).then(|| Rc::new(notes.iter().map(|note| Rc::from(*note)).collect())),
        }))
    }

    fn notes_of(value: &Value) -> Vec<String> {
        let Value::Instance(instance) = value else { panic!("not an instance: {value:?}") };
        instance.notes.iter().flat_map(|notes| notes.iter().map(ToString::to_string)).collect()
    }

    /// An error's notes go into its block and come back out of it, apart from its fields, and
    /// two errors with different notes are still equal.
    #[test]
    fn notes_survive_a_trip_through_a_block_and_dont_count_for_equality() {
        let tables = Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![lost_layout(), Rc::new(Element::Str)],
            ..Tables::default()
        };
        start(tables, crate::Inputs::default());
        let before = with(|state| state.heap.live());
        let errors = Value::list(vec![lost("no disk", &["while saving", "twice"]), lost("no disk", &[])]);
        let pushed = with(|state| state.push(errors));
        let block = super::super::lists::from_value(pushed, 0, 0, 0);
        assert_ne!(block, 0);
        with(|state| {
            let Value::List(items) = heap::read_block(state, u64::try_from(block).unwrap()).unpacked() else {
                panic!("not a list");
            };
            assert_eq!(notes_of(&items[0]), ["while saving", "twice"]);
            assert_eq!(notes_of(&items[1]), Vec::<String>::new());
            assert_eq!(items[0], items[1]);
            assert_eq!(crate::format::repr(&items[0]), "Lost(why=\"no disk\")");
            let words = state.heap.block(u64::try_from(block).unwrap()).unwrap().to_vec();
            let stride = 2;
            let items = super::super::lists::items(&words, stride);
            assert!(heap::same_words(state, &lost_layout(), &items[..2], &items[2..4]));
        });
        heap::free(block);
        with(|state| assert_eq!(state.heap.live(), before));
        finish();
    }

    /// Adding a note takes over the old list and the text, so nothing is left behind.
    #[test]
    fn adding_notes_leaves_nothing_behind() {
        begin();
        let before = with(|state| state.heap.live());
        let mut notes = 0;
        for note in ["one", "two", "three"] {
            let text = with(|state| heap::text_address(state, note));
            notes = add(notes, text);
        }
        let listed = list(notes);
        heap::free(listed);
        heap::free(notes);
        with(|state| assert_eq!(state.heap.live(), before));
        finish();
    }

    #[test]
    fn notes_are_added_last_and_shown_under_the_failure_in_order() {
        begin();
        let first = with(|state| heap::text_address(state, "while saving the file"));
        let second =
            with(|state| heap::text_address(state, "the cleanup failed:\nIoError(reason=\"disk full\")"));
        let notes = add(add(0, first), second);
        assert_ne!(notes, 0);
        let read = list(notes);
        with(|state| {
            let Value::List(items) = heap::read_block(state, u64::try_from(read).unwrap()).unpacked() else {
                panic!("not a list");
            };
            assert_eq!(items.len(), 2);
        });
        assert_eq!(
            text(shown(notes)),
            "\nnote: while saving the file\nnote: the cleanup failed:\n  IoError(reason=\"disk full\")"
        );
        finish();
    }
}
