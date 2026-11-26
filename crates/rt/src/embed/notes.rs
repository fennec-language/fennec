//! An error's notes, for native code: the word an error's layout keeps them in, `NOTES`, is 0
//! until a note is added, and a list of strings after. These take and give that word, so they
//! needn't know where an error's layout puts it, and an error with no notes costs nothing more.

use std::rc::Rc;

use fennec_syntax::{Span, Trace};

use super::heap::{read_block, release, text_address};
use super::lists::LIST_HEADER_WORDS;
use super::text::{INLINE, INLINE_BYTES};
use super::{State, with};
use crate::packed::Element;
use crate::value::Value;

/// What starts a note that's a place in the error's call chain rather than one the program added.
/// Never shown as a note or given by `notes()`.
const FRAME: &str = "\0frame ";

/// [`FRAME`]'s seven bytes as the first word of a place's note, its eighth byte left for the letter.
/// `a_places_header_holds_the_bytes_its_note_always_had` checks they're [`FRAME`]'s.
const FRAME_WORD: u64 = u64::from_le_bytes(*b"\0frame \0");

/// How many bytes hold each of a place's three numbers, seven bits to a byte, the low ones first:
/// where it starts, where it ends and how often in a row. Seven bits keep the note text.
const NUMBER_BYTES: usize = 5;

/// The most a place's count holds, where it stays.
const MOST: u64 = (1 << (7 * NUMBER_BYTES)) - 1;

/// How many words a string's header is, as a list of them holds each.
const HEADER: usize = 3;

/// How an error left a place: by `fail` or by `?`. The third is no place, but the count of those
/// left out of a long chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Left {
    Raised,
    Passed,
    Skipped,
}

/// A place in an error's call chain and how often it was left in a row, as its note says it:
/// [`FRAME`], a letter for how, then the three numbers. That's 23 bytes, as much as a string's
/// header holds itself, so a place never makes a block, and nothing is made into text until the
/// chain is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Place {
    left: Left,
    start: u32,
    end: u32,
    times: u64,
}

impl Place {
    /// The count of `times` places left out of a chain.
    fn skipped(times: u64) -> Place {
        Place { left: Left::Skipped, start: 0, end: 0, times }
    }

    /// The note's bytes, as its header holds them.
    fn bytes(self) -> [u8; INLINE_BYTES] {
        let mut bytes = [0_u8; INLINE_BYTES];
        let held = self.header().into_iter().flat_map(u64::to_le_bytes);
        for (byte, from) in bytes.iter_mut().zip(held) {
            *byte = from;
        }
        bytes
    }

    /// The place a note's bytes say, when they're one's.
    fn of(bytes: &[u8]) -> Option<Place> {
        let (letter, numbers) = bytes.strip_prefix(FRAME.as_bytes())?.split_first()?;
        if numbers.len() != 3 * NUMBER_BYTES {
            return None;
        }
        let left = match letter {
            b'r' => Left::Raised,
            b'p' => Left::Passed,
            b's' => Left::Skipped,
            _ => return None,
        };
        let mut read = numbers.as_chunks::<NUMBER_BYTES>().0.iter().map(|sevens| {
            sevens
                .iter()
                .rev()
                .try_fold(0_u64, |number, byte| (*byte < 0x80).then(|| (number << 7) | u64::from(*byte)))
        });
        let (start, end, times) = (read.next()??, read.next()??, read.next()??);
        Some(Place { left, start: u32::try_from(start).ok()?, end: u32::try_from(end).ok()?, times })
    }

    /// The note as a string.
    fn text(self) -> Rc<str> {
        Rc::from(String::from_utf8_lossy(&self.bytes()).as_ref())
    }

    /// The note as a string's header, which holds it all: [`FRAME`] and the letter in its first
    /// word, the numbers' five bytes each after them, and the inline tag in its last byte. Made
    /// straight into the three words, as every place an error leaves writes one: put together a
    /// byte at a time, it took about 550 instructions a place.
    fn header(self) -> [u64; HEADER] {
        let letter: u64 = match self.left {
            Left::Raised => b'r'.into(),
            Left::Passed => b'p'.into(),
            Left::Skipped => b's'.into(),
        };
        // Seven bits of `number` to a byte, the low ones first.
        let sevens = |number: u64| {
            (0..NUMBER_BYTES).fold(0, |bytes, at| bytes | (((number >> (7 * at)) & 0x7f) << (8 * at)))
        };
        let (start, end, times) =
            (sevens(u64::from(self.start)), sevens(u64::from(self.end)), sevens(self.times.min(MOST)));
        let tag = u64::from(INLINE) | u64::try_from(INLINE_BYTES).unwrap_or(0);
        // Bytes 0 to 7, then 8 to 15: the start's five and the end's first three, then 16 to 23:
        // the end's last two, the count's five and the tag.
        [FRAME_WORD | (letter << 56), start | (end << 40), (end >> 24) | (times << 16) | (tag << 56)]
    }

    /// The place a string's header says, when it holds one's note.
    fn of_header(words: &[u64]) -> Option<Place> {
        let mut bytes = [0_u8; 8 * HEADER];
        for (eight, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(words) {
            *eight = word.to_le_bytes();
        }
        let (tag, text) = bytes.split_last()?;
        (words.len() == HEADER && usize::from(*tag) == (usize::from(INLINE) | INLINE_BYTES))
            .then(|| Place::of(text))
            .flatten()
    }

    /// Whether an error leaving `next` left the place this is, the same way: then it's counted
    /// here, not noted again.
    fn is_where(self, next: Place) -> bool {
        self.left != Left::Skipped && (self.left, self.start, self.end) == (next.left, next.start, next.end)
    }

    /// This place, left once more.
    fn again(self) -> Place {
        Place { times: self.times.saturating_add(1).min(MOST), ..self }
    }
}

/// Whether a note is a place in its error's call chain, which only the chain shows, rather than
/// one the program added.
pub(crate) fn is_place(note: &str) -> bool {
    note.starts_with(FRAME)
}

/// The notes a notes word holds that the program added: none for 0.
fn texts(state: &State, notes: i64) -> Vec<Rc<str>> {
    every_text(state, notes).into_iter().filter(|text| !is_place(text)).collect()
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
        // A notes word that holds a block that isn't a list of them, or none this thread finds, is
        // a bug: read as no notes, they'd be dropped with nothing said, as #1843 dropped them.
        _ if super::made_up_inputs() => Vec::new(),
        _ => super::heap::runtime_bug(&format!(
            "an error's notes at {address:#x} aren't a list this thread can read"
        )),
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
///
/// Most errors are caught a call or two up and their chain never read, so this only notes the
/// place: one string's header written on the end of the list, or a count in the last one raised.
pub extern "C" fn frame(notes: i64, raised: i64, start: i64, end: i64) -> i64 {
    let left = if raised == 1 { Left::Raised } else { Left::Passed };
    // A span is two `u32`s wherever native code has one to give.
    let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end)) else { return notes };
    let place = Place { left, start, end, times: 1 };
    with(|state| noted_in_place(state, notes, place).unwrap_or_else(|| noted_anew(state, notes, place)))
}

/// What `frame` writes into a list of headers where it is, worked out before any of it is
/// written: a header changed inside the list, one taken out, and whether the place goes on the end.
struct Change {
    inside: Option<(usize, Place)>,
    taken_out: Option<usize>,
    on_the_end: bool,
}

/// What noting `place` changes in the headers `items`, by the rules `frame` gives. `None` for the
/// one case that changes two headers and takes one out, which is left to making the list anew: a
/// place left again on a chain a place too long, as it is once a place past the kept ones has
/// gone on its end. That's once for each run of one place at the end of a long chain.
fn change_for(items: &[u64], place: Place) -> Option<Change> {
    let count = items.len() / HEADER;
    let places = || {
        let headers = items.as_chunks::<HEADER>().0.iter().enumerate();
        headers.filter_map(|(at, header)| Some((at, Place::of_header(header)?)))
    };
    let last = items.rchunks_exact(HEADER).next().and_then(Place::of_header);
    if let Some(last) = last.filter(|last| last.is_where(place)) {
        if count > 2 * Trace::KEPT + 1 && places().count() > 2 * Trace::KEPT + 1 {
            return None;
        }
        return Some(Change {
            inside: Some((count.checked_sub(1)?, last.again())),
            taken_out: None,
            on_the_end: false,
        });
    }
    // One place more makes the chain too long only if the list's that long already, so a short
    // list's places aren't counted.
    if count <= 2 * Trace::KEPT || places().count() <= 2 * Trace::KEPT {
        return Some(Change { inside: None, taken_out: None, on_the_end: true });
    }
    // The first place past the kept ones becomes the count of those left out, and each after it
    // goes into that count.
    let mut after = places().skip(Trace::KEPT);
    let ((kept_at, kept), (gone_at, gone)) = (after.next()?, after.next()?);
    Some(if kept.left == Left::Skipped {
        Change {
            inside: Some((kept_at, Place::skipped(kept.times.saturating_add(gone.times)))),
            taken_out: Some(gone_at),
            on_the_end: true,
        }
    } else {
        Change { inside: Some((kept_at, Place::skipped(kept.times))), taken_out: None, on_the_end: true }
    })
}

/// `frame` on the list where it is, when native code holds strings as headers: `None` to have it
/// made anew, with nothing changed. A place on the end goes through what `fennec_list_grow` does,
/// so a list something else holds is copied and one a lock's log marked has the append noted.
/// Anything written inside the list is written only where nothing else holds it and no log has
/// marked it: a marked list changed with no note is one a crash can't put back.
fn noted_in_place(state: &mut State, notes: i64, place: Place) -> Option<i64> {
    if !state.tables.text_headers {
        return None;
    }
    let address = u64::try_from(notes).ok()?;
    if address == 0 {
        return Some(super::lists::list_of_texts(state, &place.header()));
    }
    let block = state.heap.block(address)?;
    if !matches!(super::lists::element_in(state, block), Some(Element::Text)) {
        return None;
    }
    let items = super::lists::items(block, HEADER);
    let (count, room) = (items.len() / HEADER, block.len());
    let change = change_for(items, place)?;
    if change.inside.is_some() && !state.heap.is_alone(address, block) {
        return None;
    }
    let item = |at: usize| LIST_HEADER_WORDS + at * HEADER..LIST_HEADER_WORDS + (at + 1) * HEADER;
    // One more on the end is what can move the list, so it comes before anything is written.
    let grows = change.on_the_end && change.taken_out.is_none();
    let list = if grows { super::lists::with_one_more(state, notes)? } else { notes };
    let at_end = if grows { count } else { count.checked_sub(1)? };
    if !grows && item(at_end).end > room {
        return None;
    }
    let Some(block) = u64::try_from(list).ok().and_then(|address| state.heap.block_mut(address)) else {
        return Some(list);
    };
    if let Some((at, changed)) = change.inside
        && let Some(header) = block.get_mut(item(at))
    {
        header.copy_from_slice(&changed.header());
    }
    if let Some(gone) = change.taken_out {
        // The places after the one taken out move down one, and the new one takes the last's.
        block.copy_within(item(gone).end..item(at_end).end, item(gone).start);
    }
    if change.on_the_end
        && let Some(header) = block.get_mut(item(at_end))
    {
        header.copy_from_slice(&place.header());
    }
    Some(list)
}

/// `frame` the long way: every note read out as a string, the place noted among them, and a new
/// list made of them, the old one let go of. It's how a list held as strings' blocks is changed,
/// and one something else holds or a lock's log marked, where a count inside it changes.
fn noted_anew(state: &mut State, notes: i64, place: Place) -> i64 {
    let mut all = every_text(state, notes);
    let last = all.last().and_then(|text| Place::of(text.as_bytes())).filter(|last| last.is_where(place));
    match (all.last_mut(), last) {
        (Some(text), Some(last)) => *text = last.again().text(),
        _ => all.push(place.text()),
    }
    let places: Vec<(usize, Place)> =
        all.iter().enumerate().filter_map(|(at, text)| Some((at, Place::of(text.as_bytes())?))).collect();
    if places.len() > 2 * Trace::KEPT + 1
        && let (Some(&(kept_at, kept)), Some(&(gone_at, gone))) =
            (places.get(Trace::KEPT), places.get(Trace::KEPT + 1))
    {
        // The first place past the kept ones becomes the count of those left out, and each after
        // it goes into that count.
        let counted = kept.left == Left::Skipped;
        let left_out = if counted { kept.times.saturating_add(gone.times) } else { kept.times };
        if counted {
            all.remove(gone_at);
        }
        if let Some(text) = all.get_mut(kept_at) {
            *text = Place::skipped(left_out).text();
        }
    }
    let element = super::text::string_element(state);
    let anew = Value::list(all.into_iter().map(Value::Str).collect());
    let made = super::lists::write_list(state, &element, &anew).unwrap_or(notes);
    if let Ok(address) = u64::try_from(notes)
        && address != 0
        && made != notes
    {
        release(state, address);
    }
    made
}

/// An error's call chain as its notes have it: each place and how often in a row, or `None` for
/// those left out.
fn chain(state: &State, notes: i64) -> Vec<(Option<Span>, u64)> {
    chain_in(&every_text(state, notes))
}

/// [`chain`], of an error's notes read out as strings.
pub(super) fn chain_in(notes: &[Rc<str>]) -> Vec<(Option<Span>, u64)> {
    notes
        .iter()
        .filter_map(|text| {
            let place = Place::of(text.as_bytes())?;
            let at = (place.left != Left::Skipped).then_some(Span { start: place.start, end: place.end });
            Some((at, place.times))
        })
        .collect()
}

/// `fennec_notes_traced(notes)`: the error in these notes has ended the program, which says so
/// at the place it was raised, or passed up into the program, with the rest of its call chain
/// under it. Where it went inside the standard library says nothing the program can act on, so
/// it's said where the program's own code first had it: the `?` it came into the program by.
pub extern "C" fn traced(notes: i64) {
    let places = with(|state| chain(state, notes));
    said_where_raised(&places);
}

/// What [`traced`] does, given the error's call chain: the failure this thread has now is said at
/// the first place of the chain in the program's own code, with the rest under it.
pub(super) fn said_where_raised(places: &[(Option<Span>, u64)]) {
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

    use fennec_syntax::{Span, Trace};

    use super::super::{Tables, finish, heap, lists, start, with};
    use super::{FRAME, Left, MOST, Place, add, chain, every_text, frame, list, shown, texts};
    use crate::packed::Element;
    use crate::value::{Instance, Value};

    fn begin() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
    }

    /// A run where strings are held as headers, as native code holds them, or as blocks.
    fn begin_with(headers: bool) {
        let tables =
            Tables { shapes: vec![Rc::new(Element::Str)], text_headers: headers, ..Tables::default() };
        start(tables, crate::Inputs::default());
    }

    /// A place's note as it was put together a byte at a time, before its header was made straight
    /// into words: what the words must hold.
    fn bytes_one_at_a_time(place: Place) -> Vec<u8> {
        let letter = match place.left {
            Left::Raised => b'r',
            Left::Passed => b'p',
            Left::Skipped => b's',
        };
        let numbers = [u64::from(place.start), u64::from(place.end), place.times.min(MOST)];
        let sevens = numbers.into_iter().flat_map(|number| {
            (0..super::NUMBER_BYTES).map(move |at| u8::try_from((number >> (7 * at)) & 0x7f).unwrap_or(0))
        });
        FRAME.bytes().chain([letter]).chain(sevens).collect()
    }

    /// A place's header holds the bytes its note was always put together from, a byte at a time,
    /// for every letter and every number either side of a byte's seven bits.
    #[test]
    fn a_places_header_holds_the_bytes_its_note_always_had() {
        let mut numbers: Vec<u32> = (0..32).map(|bit| 1_u32 << bit).collect();
        numbers.extend(numbers.clone().iter().map(|number| number - 1));
        numbers.extend([0x7f, 0x80, 0x3fff, 0x4000, u32::MAX, 0x5555_5555, 0xaaaa_aaaa]);
        for left in [Left::Raised, Left::Passed, Left::Skipped] {
            for (at, start) in numbers.iter().enumerate() {
                let end = numbers[(at * 7 + 3) % numbers.len()];
                for times in [1, 2, 127, 128, u64::from(u32::MAX), MOST, u64::MAX] {
                    let place = Place { left, start: *start, end, times };
                    let wanted = bytes_one_at_a_time(place);
                    assert_eq!(place.bytes().to_vec(), wanted, "{place:?}");
                    assert_eq!(place.header(), super::super::text::inline(&wanted), "{place:?}");
                }
            }
        }
    }

    /// A place's note says the place again, whatever its numbers, in 23 bytes of text that a
    /// string's header holds itself. A note of the program's isn't one.
    #[test]
    fn a_place_goes_into_its_note_and_comes_back() {
        let numbers = [0, 1, 127, 128, 16_383, 16_384, u32::MAX - 1, u32::MAX];
        for left in [Left::Raised, Left::Passed, Left::Skipped] {
            for start in numbers {
                for times in [1, 2, 127, 128, u64::from(u32::MAX), MOST] {
                    let place = Place { left, start, end: u32::MAX - start, times };
                    let bytes = place.bytes();
                    assert!(bytes.starts_with(FRAME.as_bytes()) && bytes.is_ascii(), "{bytes:?}");
                    assert_eq!(Place::of(&bytes), Some(place));
                    assert_eq!(Place::of(place.text().as_bytes()), Some(place));
                    assert_eq!(Place::of_header(&place.header()), Some(place));
                }
            }
        }
        // A count past what a note holds stays at the most, however often it's left again.
        let most = Place { left: Left::Passed, start: 3, end: 9, times: u64::MAX };
        assert_eq!(Place::of(&most.bytes()).map(|place| place.times), Some(MOST));
        assert_eq!(Place::of(&most.bytes()).map(|place| place.again().times), Some(MOST));
        for not_one in ["", "while saving", "\0frame ", "\0frame raised 12 34 1", "\0frame x234567890123456"]
        {
            assert_eq!(Place::of(not_one.as_bytes()), None, "{not_one:?}");
            assert_eq!(
                Place::of_header(&super::super::text::inline(not_one.as_bytes())),
                None,
                "{not_one:?}"
            );
        }
        // A header that's a block's address and a length is no place either.
        assert_eq!(Place::of_header(&[4096, 23, 0]), None);
    }

    /// The call chain a run of places makes, as `Trace` keeps one: each `(start, how often it's
    /// left)`, a place left `n` times being `n` calls of `frame` in a row.
    fn wanted(places: &[(u32, u64)]) -> Trace {
        let mut trace = Trace::default();
        for (start, times) in places {
            for _ in 0..*times {
                trace.push(Span { start: *start, end: start + 5 }, 1);
            }
        }
        trace
    }

    /// The chain the notes hold, as `fennec_notes_traced` reads it into a `Trace`.
    fn held(notes: i64) -> Trace {
        let mut trace = Trace::default();
        for (place, times) in with(|state| chain(state, notes)) {
            match place {
                Some(place) => trace.push(place, times),
                None => trace.skip(times),
            }
        }
        trace
    }

    /// Notes with these places put in them by `frame`, a note of the program's added after every
    /// `note_every` places.
    fn noted(places: &[(u32, u64)], note_every: usize) -> i64 {
        let mut notes = 0;
        for (number, (start, times)) in places.iter().enumerate() {
            for _ in 0..*times {
                notes = frame(notes, i64::from(number == 0), i64::from(*start), i64::from(start + 5));
            }
            if note_every > 0 && (number + 1) % note_every == 0 {
                let text = with(|state| heap::text_address(state, &format!("a note after place {number}")));
                notes = add(notes, text);
            }
        }
        notes
    }

    /// Chains of every shape: one place, one left over and over, two taking turns far past what's
    /// kept, a long run of different ones, a long one ending in one repeated.
    fn chains() -> Vec<Vec<(u32, u64)>> {
        let different = |count: u32| (0..count).map(|at| (10 * at, 1)).collect::<Vec<_>>();
        let turns = |count: u32| (0..count).map(|at| (10 * (at % 2), 1)).collect::<Vec<_>>();
        let mut ending = different(40);
        ending.push((7, 9));
        let mut counted = different(33);
        counted.extend([(1, 3), (2, 1), (2, 4), (3, 2)]);
        vec![
            vec![(1, 1)],
            vec![(1, 500)],
            vec![(1, 2), (2, 1), (1, 3)],
            different(16),
            different(32),
            different(33),
            different(34),
            different(35),
            different(100),
            turns(200),
            ending,
            counted,
        ]
    }

    /// The places a chain's notes keep, by the rule `frame` gives, worked out the plain way on a
    /// list, a note of the program's after every `note_every` places: where each place starts, or
    /// `None` for the count of those left out, and how often.
    fn kept(places: &[(u32, u64)], note_every: usize) -> Vec<(Option<u32>, u64)> {
        // A place is how it was left and where it starts, or `None` for the count; a note of the
        // program's is `None` altogether.
        type Noted = Option<(Option<(bool, u32)>, u64)>;
        let mut all: Vec<Noted> = Vec::new();
        for (number, (start, times)) in places.iter().enumerate() {
            for _ in 0..*times {
                let here = Some((number == 0, *start));
                match all.last_mut() {
                    Some(Some((last, count))) if *last == here => *count += 1,
                    _ => all.push(Some((here, 1))),
                }
                let frames: Vec<usize> =
                    all.iter().enumerate().filter(|(_, note)| note.is_some()).map(|(at, _)| at).collect();
                if frames.len() > 2 * Trace::KEPT + 1 {
                    let (count, gone) = (frames[Trace::KEPT], frames[Trace::KEPT + 1]);
                    let calls = |at: usize| all[at].map_or(1, |(_, times)| times);
                    let counted = matches!(all[count], Some((None, _)));
                    let left_out = if counted { calls(count) + calls(gone) } else { calls(count) };
                    if counted {
                        all.remove(gone);
                    }
                    all[count] = Some((None, left_out));
                }
            }
            if note_every > 0 && (number + 1) % note_every == 0 {
                all.push(None);
            }
        }
        all.into_iter().flatten().map(|(place, times)| (place.map(|(_, start)| start), times)).collect()
    }

    /// `frame` keeps a chain by its rule, with strings held as headers, where it changes the list
    /// where it is, and as blocks, where it makes the list anew. The two hold the same notes, the
    /// program's own come back in order with no place among them, and nothing is left behind.
    #[test]
    fn a_chain_is_the_same_noted_in_place_and_made_anew() {
        for places in chains() {
            for note_every in [0, 1, 7] {
                let mut read = Vec::new();
                for headers in [true, false] {
                    begin_with(headers);
                    let before = with(|state| state.heap.live());
                    let notes = noted(&places, note_every);
                    let held: Vec<(Option<u32>, u64)> = with(|state| chain(state, notes))
                        .into_iter()
                        .map(|(place, times)| (place.map(|place| place.start), times))
                        .collect();
                    assert_eq!(held, kept(&places, note_every), "{headers} {note_every} {places:?}");
                    let own: Vec<String> =
                        with(|state| texts(state, notes)).iter().map(ToString::to_string).collect();
                    let added = places.len().checked_div(note_every).unwrap_or(0);
                    assert_eq!(own.len(), added, "{own:?}");
                    assert!(own.iter().all(|note| note.starts_with("a note after place ")), "{own:?}");
                    read.push(with(|state| every_text(state, notes)));
                    let listed = list(notes);
                    heap::free(listed);
                    heap::free(notes);
                    with(|state| assert_eq!(state.heap.live(), before, "{headers} {note_every} {places:?}"));
                    finish();
                }
                assert_eq!(read.first(), read.get(1), "{note_every} {places:?}");
            }
        }
    }

    /// A place on the end of a short chain, and one left again, change the list where it is: the
    /// notes word stays the same once the list has room, and no block is made.
    #[test]
    fn a_place_is_written_into_the_list_where_it_is() {
        begin_with(true);
        let first = frame(0, 1, 10, 15);
        assert_ne!(first, 0);
        let made = with(|state| state.heap.live());
        let mut notes = first;
        for at in 0..3 {
            notes = frame(notes, 0, 20 + 10 * at, 25 + 10 * at);
        }
        let grown = notes;
        for _ in 0..1000 {
            notes = frame(notes, 0, 40, 45);
        }
        assert_eq!(notes, grown);
        with(|state| assert_eq!(state.heap.live(), made));
        assert_eq!(held(notes), wanted(&[(10, 1), (20, 1), (30, 1), (40, 1001)]));
        heap::free(notes);
        finish();
    }

    /// A list something else holds, or one a lock's log has marked, is never written inside: a
    /// place left again there makes the list anew, and the other holder's notes stay as they were.
    #[test]
    fn a_list_held_twice_or_marked_is_made_anew_for_a_count() {
        for marked in [false, true] {
            begin_with(true);
            let before = with(|state| state.heap.live());
            let held_twice = noted(&[(10, 1), (20, 2)], 0);
            let address = u64::try_from(held_twice).unwrap();
            with(|state| if marked { lists::mark(state, address) } else { heap::retain(state, address) });
            let as_it_was = with(|state| every_text(state, held_twice));
            let again = frame(held_twice, 0, 20, 25);
            assert_ne!(again, held_twice);
            assert_eq!(with(|state| every_text(state, held_twice)), as_it_was);
            assert_eq!(held(again), wanted(&[(10, 1), (20, 3)]));
            heap::free(again);
            with(
                |state| {
                    if marked { lists::unmark(state, address, false) } else { heap::release(state, address) }
                },
            );
            with(|state| assert_eq!(state.heap.live(), before, "marked: {marked}"));
            finish();
        }
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
        let block = lists::from_value(pushed, 0, 0, 0);
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
            let items = lists::items(&words, stride);
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
