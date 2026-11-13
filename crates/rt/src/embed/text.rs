//! A string held in place as its 24-byte header, as LLVM's code holds one: up to 23 bytes
//! of text in the header itself, or the address of a string's block. The last byte says
//! which, so the runtime never has to guess:
//!
//! - **Inline:** the bytes, zero after the end, and `0x80 | length` in byte 23.
//! - **In a block:** word 0 is the block's address and word 1 the length in bytes, and byte 23
//!   is 0.
//!
//! A string of 23 bytes or fewer is always inline, so two equal strings always have the same
//! form, and two inline ones are equal exactly when their words are. Native code hands a
//! header to the runtime as its three words, never as an address to read.

use std::rc::Rc;

use super::heap::{TEXT_HEADER_WORDS, block_text, release, retain, text_from_bytes};
use super::{State, with};
use crate::packed::Element;

/// The most bytes a header holds itself.
pub const INLINE_BYTES: usize = 23;

/// What byte 23 holds, above the length, for an inline string.
pub const INLINE: u8 = 0x80;

/// A string's header, as the runtime gives one back to native code.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Text {
    pub words: [u64; 3],
}

/// The empty string's header, inline: what a function giving back a header gives when it has
/// nothing else to, so native code never reads one pointing at no block.
impl Default for Text {
    fn default() -> Text {
        Text { words: inline(&[]) }
    }
}

/// What byte 23 of a header holds.
fn tag(words: &[u64; 3]) -> u8 {
    let [.., last] = *words;
    let [.., byte] = last.to_le_bytes();
    byte
}

/// The block a header points at, when it's in one.
pub(in crate::embed) fn block_of(words: &[u64; 3]) -> Option<u64> {
    let [first, ..] = *words;
    (tag(words) & INLINE == 0).then_some(first)
}

/// The bytes an inline header holds, when it's inline.
pub(super) fn inline_bytes(words: &[u64; 3]) -> Option<Vec<u8>> {
    let tag = tag(words);
    if tag & INLINE == 0 {
        return None;
    }
    let length = usize::from(tag & !INLINE).min(INLINE_BYTES);
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).take(length).collect();
    Some(bytes)
}

/// An inline header holding `length` bytes, 8 or fewer, packed in a word with zeros after them.
pub(super) fn inline_word(word: u64, length: u8) -> [u64; 3] {
    [word, 0, u64::from(INLINE | length) << 56]
}

/// An inline header holding `bytes`, which are 23 or fewer, packed a word at a time.
pub(super) fn inline(bytes: &[u8]) -> [u64; 3] {
    let length = bytes.len().min(INLINE_BYTES);
    let mut words = [0_u64; 3];
    for (word, chunk) in words.iter_mut().zip(bytes.get(..length).unwrap_or_default().chunks(8)) {
        let mut eight = [0_u8; 8];
        eight.get_mut(..chunk.len()).unwrap_or_default().copy_from_slice(chunk);
        *word = u64::from_le_bytes(eight);
    }
    // The tag goes in the last byte, which 23 bytes leave free.
    let [first, second, mut last] = words;
    last |= u64::from(INLINE | u8::try_from(length).unwrap_or(0)) << 56;
    [first, second, last]
}

/// The header of `packed`, whose first `length` bytes, 23 or fewer, are a text's, with its tag
/// put in the last.
fn packed_header(mut packed: [u8; 24], length: usize) -> [u64; 3] {
    if let Some(last) = packed.last_mut() {
        *last = INLINE | u8::try_from(length.min(INLINE_BYTES)).unwrap_or(0);
    }
    let word = |at: usize| {
        let mut eight = [0_u8; 8];
        if let Some(from) = packed.get(at * 8..at * 8 + 8) {
            eight.copy_from_slice(from);
        }
        u64::from_le_bytes(eight)
    };
    [word(0), word(1), word(2)]
}

/// The header of a string's text, inline when it fits, making a block when it doesn't.
pub(super) fn header(state: &mut State, text: &str) -> [u64; 3] {
    if text.len() <= INLINE_BYTES {
        return inline(text.as_bytes());
    }
    let address = super::heap::text_address(state, text);
    [address.cast_unsigned(), u64::try_from(text.len()).unwrap_or(0), 0]
}

/// The text a header holds, from itself or from its block.
pub(super) fn text_of(state: &State, words: &[u64; 3]) -> String {
    if let Some(bytes) = inline_bytes(words) {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let block = block_of(words).and_then(|address| state.heap.block(address));
    block.filter(|block| super::heap::is_text(state, block)).map_or_else(String::new, block_text)
}

/// The kind of item a list of strings the runtime makes holds: headers, when native code holds
/// strings as headers, and blocks' addresses otherwise.
pub(super) fn string_element(state: &State) -> Rc<Element> {
    Rc::new(if state.tables.text_headers { Element::Text } else { Element::Str })
}

/// Whether the string held as `words`, a header, is the one in the block at `wanted`, with nothing
/// made to compare them: their lengths, then the words their bytes take up. An inline header packs
/// its bytes as a block does, with zeros after them, so equal bytes are equal words.
pub(super) fn header_is(state: &State, words: &[u64; 3], wanted: u64) -> bool {
    if tag(words) & INLINE == 0 {
        return block_of(words).is_some_and(|address| super::heap::same(state, address, wanted));
    }
    let Some(block) = state.heap.block(wanted) else { return false };
    let length = usize::from(tag(words) & !INLINE).min(INLINE_BYTES);
    if block.get(2).copied() != u64::try_from(length).ok() {
        return false;
    }
    let [first, second, last] = *words;
    let mine = [first, second, last & (u64::MAX >> 8)];
    let used = length.div_ceil(8);
    let theirs = block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + used);
    mine.get(..used) == theirs
}

/// The texts of a list of headers' items, laid out three words each.
pub(super) fn texts(state: &State, words: &[u64]) -> Vec<String> {
    words.as_chunks::<3>().0.iter().map(|header| text_of(state, header)).collect()
}

/// `fennec_text_block(a, b, c)`: a string's block, owned once, for the runtime's string calls,
/// which take blocks. An inline string is copied into a new block; a string in a block gives
/// that block, with one more owner.
pub extern "C" fn block(a: u64, b: u64, c: u64) -> i64 {
    let words = [a, b, c];
    super::made_with("fennec_text_block", |state| {
        if tag(&words) & INLINE != 0 {
            return block_of_inline(state, &words);
        }
        let address = block_of(&words).unwrap_or(0);
        retain(state, address);
        address.cast_signed()
    })
}

/// A header's bytes where they are, as `bytes_of` gives them, and how many characters they make:
/// a block keeps its count, and an inline header's are its bytes that don't carry on one before
/// them, as its bytes are a string's, already UTF-8.
pub(super) fn measured<'a>(state: &'a State, words: &'a [u64; 3]) -> (&'a [u64], usize, u64) {
    let (bytes, length) = bytes_of(state, words);
    let chars = if let Some(address) = block_of(words) {
        state.heap.block(address).and_then(|block| block.get(3)).copied().unwrap_or(0)
    } else {
        let packed = bytes.iter().flat_map(|word| word.to_le_bytes()).take(length);
        u64::try_from(packed.filter(|byte| byte & 0xC0 != 0x80).count()).unwrap_or(0)
    };
    (bytes, length, chars)
}

/// A new block owned once holding an inline header's text, its words copied across whole and its
/// characters counted eight bytes at a time: a header's bytes are a string's, already UTF-8, so its
/// characters are its bytes that don't carry on one before them. One ASCII character is the block
/// every such string shares.
fn block_of_inline(state: &mut State, words: &[u64; 3]) -> i64 {
    let length = usize::from(tag(words) & !INLINE).min(INLINE_BYTES);
    let [first, ..] = *words;
    if length == 1
        && let Some(address) =
            usize::try_from(first & 0xff).ok().and_then(|byte| state.tables.ascii.get(byte))
    {
        return i64::try_from(*address).unwrap_or(0);
    }
    let chars = super::strings::chars_before(words, length);
    let used = length.div_ceil(8);
    let room = used.max(1);
    // The header and each word with bytes in it are written here, and only an empty string's one
    // word is left to be zero.
    let mut block = state.heap.fresh_written(TEXT_HEADER_WORDS + room, TEXT_HEADER_WORDS + used);
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    let header = [1, state.tables.str_shape, number(length), number(chars), number(room * 8), 0];
    if let Some(start) = block.get_mut(..TEXT_HEADER_WORDS) {
        start.copy_from_slice(&header);
    }
    if let Some(text) = block.get_mut(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + used)
        && let Some(from) = words.get(..used)
    {
        text.copy_from_slice(from);
        // Past the last byte, the tag among them, is zero, as native code compares words whole.
        let tail = u32::try_from(length % 8 * 8).unwrap_or(0);
        if tail > 0
            && let Some(last) = text.last_mut()
        {
            *last &= u64::MAX >> (64 - tail);
        }
    }
    state.heap.insert(block)
}

/// A header's bytes where they are: in its own words when it's inline, and in its block otherwise.
pub(super) fn bytes_of<'a>(state: &'a State, words: &'a [u64; 3]) -> (&'a [u64], usize) {
    match block_of(words) {
        Some(address) => super::strings::bytes_in_block(state, address.cast_signed()),
        None => (&words[..], usize::from(tag(words) & !INLINE).min(INLINE_BYTES)),
    }
}

/// `fennec_text_contains(text, part)`: `part in text` for two headers, each three words, which
/// copies neither.
pub extern "C" fn contains(t0: u64, t1: u64, t2: u64, p0: u64, p1: u64, p2: u64) -> i8 {
    with(|state| {
        let (text, part) = ([t0, t1, t2], [p0, p1, p2]);
        i8::from(super::strings::bytes_contain(bytes_of(state, &text), bytes_of(state, &part)))
    })
}

/// A header's bytes, copied out.
fn bytes_vec(state: &State, words: &[u64; 3]) -> Vec<u8> {
    let (words, length) = bytes_of(state, words);
    words.iter().flat_map(|word| word.to_le_bytes()).take(length).collect()
}

/// How many characters a header's text has: a block keeps its count, and an inline header's are
/// its bytes that don't carry on one before them, as its bytes are a string's, already UTF-8.
fn chars_of(state: &State, words: &[u64; 3]) -> u64 {
    if let Some(address) = block_of(words) {
        return state.heap.block(address).and_then(|block| block.get(3)).copied().unwrap_or(0);
    }
    let count = bytes_vec(state, words).iter().filter(|byte| **byte & 0xC0 != 0x80).count();
    u64::try_from(count).unwrap_or(0)
}

/// `fennec_str_append_header(text, piece)`: `text += piece` for a piece given as its header, inline
/// or holding a block, so native code needn't make an inline piece a block first. It takes over
/// the owner `text` had and gives back the joined string's, as `fennec_str_append` does; the piece
/// is only read.
pub extern "C" fn append_header(text: i64, h0: u64, h1: u64, h2: u64) -> i64 {
    super::made_with("fennec_str_append_header", |state| {
        let piece = [h0, h1, h2];
        let chars = chars_of(state, &piece);
        // Copied first: `text += text` reads the block it writes.
        let mut copied = std::mem::take(&mut state.heap.scratch);
        copied.clear();
        let (words, length) = bytes_of(state, &piece);
        copied.extend_from_slice(words.get(..length.div_ceil(8)).unwrap_or_default());
        let grown = super::strings::grow_by(state, text, &copied, length, chars);
        state.heap.scratch = copied;
        grown.unwrap_or(text)
    })
}

/// `fennec_str_find_header(text, part)`: `text.find(part)` for two headers, which character `part`
/// first starts at, or -1 when it isn't there, looked for where both are, as `find` does.
pub extern "C" fn find_header(t0: u64, t1: u64, t2: u64, p0: u64, p1: u64, p2: u64) -> i64 {
    with(|state| {
        let (text, part) = ([t0, t1, t2], [p0, p1, p2]);
        super::strings::find_in(bytes_of(state, &text), bytes_of(state, &part))
    })
}

/// `fennec_str_same_at(text, start, word)`: whether `text`'s bytes from byte `start` on begin
/// with all of `word`'s, for two headers. False when `start` is negative or `word` would run past
/// `text`'s end. Compared eight bytes at a time, with neither copied, for matching a JSON field's
/// name where it's found in what's read.
pub extern "C" fn same_at(t0: u64, t1: u64, t2: u64, start: i64, w0: u64, w1: u64, w2: u64) -> i8 {
    with(|state| {
        let (text, word) = ([t0, t1, t2], [w0, w1, w2]);
        i8::from(
            usize::try_from(start)
                .is_ok_and(|start| words_same_at(bytes_of(state, &text), start, bytes_of(state, &word))),
        )
    })
}

/// [`same_at`] on two texts' words, each with its length in bytes.
fn words_same_at(
    (text, text_length): (&[u64], usize),
    start: usize,
    (word, length): (&[u64], usize),
) -> bool {
    if start.checked_add(length).is_none_or(|end| end > text_length) {
        return false;
    }
    let mut at = 0;
    while at < length {
        let left = length - at;
        // The bytes past the word's end in its last eight aren't compared.
        let mask = if left >= 8 { u64::MAX } else { (1_u64 << (left * 8)) - 1 };
        let wanted = word.get(at / 8).copied().unwrap_or(0);
        if (eight_bytes_at(text, start + at) ^ wanted) & mask != 0 {
            return false;
        }
        at += 8;
    }
    true
}

/// The eight bytes of `words` from byte `at` on, as a word, zero past the words' end. Bytes are
/// packed into words in order from each word's lowest byte, as a block and an inline header hold
/// them.
fn eight_bytes_at(words: &[u64], at: usize) -> u64 {
    let (number, shift) = (at / 8, (at % 8) * 8);
    let low = words.get(number).copied().unwrap_or(0) >> shift;
    if shift == 0 {
        return low;
    }
    low | (words.get(number + 1).copied().unwrap_or(0) << (64 - shift))
}

/// `fennec_text_starts_with(text, part)`: `text.startswith(part)` for two headers.
pub extern "C" fn starts_with(t0: u64, t1: u64, t2: u64, p0: u64, p1: u64, p2: u64) -> i8 {
    with(|state| i8::from(bytes_vec(state, &[t0, t1, t2]).starts_with(&bytes_vec(state, &[p0, p1, p2]))))
}

/// `fennec_text_ends_with(text, part)`: `text.endswith(part)` for two headers.
pub extern "C" fn ends_with(t0: u64, t1: u64, t2: u64, p0: u64, p1: u64, p2: u64) -> i8 {
    with(|state| i8::from(bytes_vec(state, &[t0, t1, t2]).ends_with(&bytes_vec(state, &[p0, p1, p2]))))
}

/// `fennec_text_of(address)`: the header of a string's block, taking over the owner given.
/// Text of 23 bytes or fewer comes back inline, letting go of the block.
pub extern "C" fn of(address: i64) -> Text {
    with(|state| {
        let at = address.cast_unsigned();
        let Some(block) = state.heap.block(at).filter(|block| super::heap::is_text(state, block)) else {
            return Text { words: inline(&[]) };
        };
        let length = block.get(2).and_then(|length| usize::try_from(*length).ok()).unwrap_or(0);
        if length > INLINE_BYTES {
            return Text { words: [at, u64::try_from(length).unwrap_or(0), 0] };
        }
        let bytes: Vec<u8> = block
            .get(TEXT_HEADER_WORDS..)
            .unwrap_or_default()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .take(length)
            .collect();
        let words = inline(&bytes);
        release(state, at);
        Text { words }
    })
}

/// `fennec_str_append_int(text, n)`: `text += str(n)`, taking over the owner `text` had and giving
/// back the joined string's. The int's digits are written where they're worked out, on the stack,
/// and go straight onto the end of `text`: in place when it has no other owner and its block has
/// room, and otherwise into a new block with room to double, as any `+=` does. No text is made of
/// the digits on the way.
pub extern "C" fn append_int(text: i64, n: i64) -> i64 {
    super::made_with("fennec_str_append_int", |state| {
        let Text { words } = int_header(n);
        let length = usize::from(tag(&words) & !INLINE).min(INLINE_BYTES);
        // The header's words without its tag are the digits, packed as a block packs them.
        let [first, second, last] = words;
        let digits = [first, second, last & (u64::MAX >> 8)];
        let added = digits.get(..length.div_ceil(8)).unwrap_or_default();
        // An int's digits and sign are ASCII, a character each.
        let chars = u64::try_from(length).unwrap_or(0);
        super::strings::grow_by(state, text, added, length, chars).unwrap_or(text)
    })
}

/// `fennec_int_header(n)`: an int's text as a header, which its digits always fit inline, with
/// no block.
pub extern "C" fn int_header(n: i64) -> Text {
    // The digits are written where they go in the header, from the last, once it's known how
    // many there are.
    let mut rest = n.unsigned_abs();
    let mut count = 1;
    let mut left = rest / 10;
    while left > 0 {
        count += 1;
        left /= 10;
    }
    let length = count + usize::from(n < 0);
    let mut packed = [0_u8; 24];
    if n < 0
        && let Some(sign) = packed.first_mut()
    {
        *sign = b'-';
    }
    for digit in packed.iter_mut().take(length).rev().take(count) {
        *digit = b'0' + u8::try_from(rest % 10).unwrap_or(0);
        rest /= 10;
    }
    Text { words: packed_header(packed, length) }
}

/// An int's decimal digits, with a `-` before them when it's negative, written into the end of
/// `digits`, which the longest of them fills.
pub(super) fn decimal(n: i64, digits: &mut [u8; 20]) -> &[u8] {
    let mut at = digits.len();
    let mut rest = n.unsigned_abs();
    loop {
        at -= 1;
        if let (Some(digit), Ok(low)) = (digits.get_mut(at), u8::try_from(rest % 10)) {
            *digit = b'0' + low;
        }
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if n < 0 {
        at -= 1;
        if let Some(sign) = digits.get_mut(at) {
            *sign = b'-';
        }
    }
    digits.get(at..).unwrap_or_default()
}

/// The few bytes a float's text takes, written where they're kept.
#[derive(Default)]
struct Digits {
    bytes: [u8; 24],
    length: usize,
}

impl std::fmt::Write for Digits {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.length + text.len() > INLINE_BYTES {
            return Err(std::fmt::Error);
        }
        for (to, from) in self.bytes.iter_mut().skip(self.length).zip(text.bytes()) {
            *to = from;
        }
        self.length += text.len();
        Ok(())
    }
}

/// A float's text as `format::float` writes it, inline, when it's written with no exponent and
/// fits; `None` for the rest.
#[expect(clippy::float_cmp, reason = "checking for a whole number is an exact comparison")]
fn plain_float(x: f64) -> Option<[u64; 3]> {
    use std::fmt::Write as _;
    if !x.is_finite() || (x != 0.0 && !(1e-4..1e16).contains(&x.abs())) {
        return None;
    }
    if let Some(words) = few_places(x) {
        return Some(words);
    }
    let mut digits = Digits::default();
    if x == x.trunc() { write!(digits, "{x}.0") } else { write!(digits, "{x}") }.ok()?;
    Some(packed_header(digits.bytes, digits.length))
}

/// A float's text when it's short, as most saved floats are: `m` over `10^places` for the fewest
/// places with an `m` that reads back as `x`. Both are exact as floats and division rounds
/// correctly, so `m / 10^places == x` says exactly that the text `m` with `places` decimals reads
/// as `x`; with fewer places none does, so its digits are the shortest that do, which is what
/// `{x}` prints. `None` when it needs more places, or two `m` would do, and for the rest.
#[expect(clippy::float_cmp, reason = "the text reads back as exactly this float")]
fn few_places(x: f64) -> Option<[u64; 3]> {
    const POWERS: [f64; 9] = [1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8];
    // Below 2^53, every whole number is exact as a float.
    const EXACT: f64 = 9_007_199_254_740_992.0;
    let size = x.abs();
    for (places, power) in POWERS.iter().enumerate() {
        let scaled = size * power;
        if scaled >= EXACT - 2.0 {
            return None;
        }
        let near = scaled.round();
        let mut found = None;
        for whole in [near - 1.0, near, near + 1.0] {
            if whole >= 0.0 && whole / power == size {
                if found.is_some() {
                    return None;
                }
                found = Some(whole);
            }
        }
        if let Some(whole) = found {
            return Some(with_places(whole, places, x.is_sign_negative()));
        }
    }
    None
}

/// The text of `whole` with its last `places` digits after a decimal point, or with `.0` when
/// there are none: as `format::float` writes a float.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "`few_places` gives a whole number from 0 to 2^53"
)]
fn with_places(whole: f64, places: usize, negative: bool) -> [u64; 3] {
    let mut digits = [0_u8; 20];
    let mut count = 0;
    let mut rest = whole as u64;
    // At least one digit before the point, and `places` after it.
    while rest > 0 || count <= places {
        if let Some(digit) = digits.get_mut(count) {
            *digit = b'0' + u8::try_from(rest % 10).unwrap_or(0);
        }
        rest /= 10;
        count += 1;
    }
    let mut packed = [0_u8; 24];
    let mut length = 0;
    let mut put = |byte: u8| {
        if let Some(at) = packed.get_mut(length) {
            *at = byte;
        }
        length += 1;
    };
    if negative {
        put(b'-');
    }
    for at in (0..count).rev() {
        if at + 1 == places {
            put(b'.');
        }
        put(digits.get(at).copied().unwrap_or(b'0'));
    }
    if places == 0 {
        put(b'.');
        put(b'0');
    }
    packed_header(packed, length)
}

/// `fennec_code_header(code)`: the character with a code as a header. A number that isn't a
/// character's code gives the replacement character, which `std.json` never asks for.
pub extern "C" fn code_header(code: i64) -> Text {
    let found = u32::try_from(code).ok().and_then(char::from_u32).unwrap_or(char::REPLACEMENT_CHARACTER);
    Text { words: inline(found.encode_utf8(&mut [0; 4]).as_bytes()) }
}

/// `fennec_float_header(x)`: a float's text as it prints, as a header, inline unless it's the
/// longest there is. One written with no exponent, as most are, is made in place, with no text
/// made on the way.
pub extern "C" fn float_header(x: f64) -> Text {
    if let Some(words) = plain_float(x) {
        return Text { words };
    }
    header_of(&crate::format::display(&crate::value::Value::Float(x)))
}

/// A string's header for `text`: inline when it fits, and otherwise a block's.
fn header_of(text: &str) -> Text {
    if text.len() <= INLINE_BYTES {
        return Text { words: inline(text.as_bytes()) };
    }
    with(|state| {
        let address = text_from_bytes(state, text.as_bytes(), text.len()).cast_unsigned();
        Text { words: [address, u64::try_from(text.len()).unwrap_or(0), 0] }
    })
}

/// `fennec_u64_header(n)`: a `u64`'s text, unsigned, as a header. It always fits inline.
pub extern "C" fn u64_header(n: i64) -> Text {
    header_of(&n.cast_unsigned().to_string())
}

/// `fennec_f32_header(x)`: an `f32`'s text, the shortest that reads back as it, as a header.
pub extern "C" fn f32_header(x: f64) -> Text {
    header_of(&crate::format::float32(x))
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, lists, start, strings, with};
    use super::{INLINE, block_of, inline_bytes};
    use crate::packed::Element;
    use crate::value::Value;

    fn begin() {
        let text = Rc::new(Element::Text);
        let optional = Rc::new(Element::Maybe(Rc::clone(&text)));
        let tables = Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![text, optional],
            text_headers: true,
            ..Tables::default()
        };
        start(tables, crate::Inputs::default());
    }

    fn header(text: &str) -> [u64; 3] {
        with(|state| Some(super::header(state, text))).unwrap()
    }

    fn text_of(words: [u64; 3]) -> String {
        with(|state| Some(super::text_of(state, &words))).unwrap()
    }

    fn live() -> usize {
        with(|state| state.heap.live())
    }

    /// `same_at` says what comparing the bytes one at a time says, for every start and every word
    /// that's a piece of the text or one byte off from one: texts inline and in blocks, of 23 and
    /// 24 bytes, of characters of one to four bytes, starts before, inside and past the end, and
    /// words running past the end.
    #[test]
    fn a_word_is_the_same_at_a_place_exactly_when_its_bytes_are() {
        begin();
        let texts = [
            "",
            "a",
            "\"user\": 1",
            "aaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbb",
            "{\"user\": 12, \"item\": \"arrow-12\", \"quantity\": 13}",
            "caf\u{e9} \u{65e5}\u{672c} \u{1f98a} long enough to be held in a block",
        ];
        for text in texts {
            let held = header(text);
            let bytes = text.as_bytes();
            let mut words: Vec<Vec<u8>> = Vec::new();
            for from in 0..=bytes.len() {
                for to in from..=bytes.len().min(from + 30) {
                    let piece = bytes.get(from..to).unwrap_or_default().to_vec();
                    let mut changed = piece.clone();
                    if let Some(last) = changed.last_mut() {
                        *last ^= 1;
                    }
                    words.push(piece);
                    words.push(changed);
                }
            }
            words.push(b"past the end of every text here, and longer".to_vec());
            for word in &words {
                let Ok(word_text) = std::str::from_utf8(word) else { continue };
                let word_held = header(word_text);
                for start in -1..=i64::try_from(bytes.len()).unwrap() + 1 {
                    let wanted = usize::try_from(start)
                        .ok()
                        .and_then(|start| bytes.get(start..start + word.len()))
                        .is_some_and(|there| there == word.as_slice());
                    let [t0, t1, t2] = held;
                    let [w0, w1, w2] = word_held;
                    let said = super::same_at(t0, t1, t2, start, w0, w1, w2) == 1;
                    assert_eq!(said, wanted, "{word_text:?} at {start} in {text:?}");
                }
            }
        }
        finish();
    }

    /// An int added onto a text gives the text with the int's digits after it: for 0, negative
    /// numbers and both ends of what an int holds, onto an empty text, one with room, one at
    /// exactly its room, and one past it. A text with one owner and room grows where it is, and one
    /// that's shared is left as it was, with the sum a text of its own.
    #[test]
    fn an_int_added_onto_a_text_is_its_digits_after_it() {
        begin();
        let text_at = |address: i64| {
            with(|state| Some(heap::block_text(state.heap.block(address.cast_unsigned())?))).unwrap()
        };
        let room_of =
            |address: i64| with(|state| state.heap.block(address.cast_unsigned())?.get(4).copied()).unwrap();
        let numbers = [0, 7, -7, 10, -1_000_000, 123_456_789, i64::MAX, i64::MIN];
        // Texts of 0, 5, 8, 16, 30 and 64 bytes, the last two of characters past ASCII too.
        let starts = [
            "",
            "user=",
            "eightlen",
            "sixteen bytes it",
            "caf\u{e9} \u{65e5}\u{672c} of thirty bytes!!",
            &"x".repeat(64),
        ];
        for start in starts {
            for n in numbers {
                let made = with(|state| heap::text_address(state, start));
                let joined = super::append_int(made, n);
                assert_eq!(text_at(joined), format!("{start}{n}"), "{start:?} and {n}");
                let chars = with(|state| state.heap.block(joined.cast_unsigned())?.get(3).copied()).unwrap();
                assert_eq!(chars, u64::try_from(format!("{start}{n}").chars().count()).unwrap());
                // Again and again onto what it gave: past its room, then within the doubled room.
                let mut grown = joined;
                let mut wanted = format!("{start}{n}");
                for _ in 0..6 {
                    let before = (grown, room_of(grown), wanted.len());
                    grown = super::append_int(grown, n);
                    wanted.push_str(&n.to_string());
                    assert_eq!(text_at(grown), wanted);
                    // With room for the digits it grows where it is, and without, it moves.
                    let fitted = u64::try_from(wanted.len()).unwrap() <= before.1;
                    assert_eq!(
                        grown == before.0,
                        fitted,
                        "{wanted:?} from {} bytes in {}",
                        before.2,
                        before.1
                    );
                }
                heap::free_text(grown);
            }
        }
        // A text filled to exactly its room, then one digit more.
        let exact = with(|state| heap::text_address(state, "12345678"));
        assert_eq!(room_of(exact), 8);
        let more = super::append_int(exact, 9);
        assert_eq!(text_at(more), "123456789");
        assert_ne!(more, exact);
        heap::free_text(more);
        // A shared text is left as it was: the sum is a text of its own, and the other owner's
        // still reads as before.
        let shared = with(|state| heap::text_address(state, "shared text with room to spare? "));
        with(|state| heap::retain(state, shared.cast_unsigned()));
        let sum = super::append_int(shared, -42);
        assert_ne!(sum, shared);
        assert_eq!(text_at(shared), "shared text with room to spare? ");
        assert_eq!(text_at(sum), "shared text with room to spare? -42");
        heap::free_text(sum);
        heap::free_text(shared);
        assert_eq!(live(), 0);
        finish();
    }

    /// An int's text, as a header or as a string's block, is what Rust writes for it, at both ends
    /// of what an int holds.
    #[test]
    fn an_int_s_text_is_its_decimal_digits() {
        begin();
        for n in [0, 7, -7, 10, -1_000_000, i64::MAX, i64::MIN] {
            assert_eq!(text_of(super::int_header(n).words), n.to_string());
            let block = strings::int_text(n);
            let made = with(|state| Some(heap::block_text(state.heap.block(block.cast_unsigned())?)));
            assert_eq!(made.as_deref(), Some(n.to_string().as_str()));
        }
        finish();
    }

    /// Numbers come back as headers. All of them but the longest floats make no block.
    #[test]
    fn a_numbers_text_needs_no_block_unless_its_too_long_for_a_header() {
        begin();
        let before = live();
        for (n, shown) in [
            (0, "0"),
            (7, "7"),
            (-7, "-7"),
            (1200, "1200"),
            (i64::MAX, "9223372036854775807"),
            (i64::MIN, "-9223372036854775808"),
        ] {
            assert_eq!(super::int_header(n).words, super::inline(shown.as_bytes()));
        }
        assert_eq!(super::float_header(1.5).words, super::inline(b"1.5"));
        assert_eq!(super::float_header(3.0).words, super::inline(b"3.0"));
        assert_eq!(live(), before);
        let longest = "-1.7976931348623157e+308";
        let words = super::float_header(f64::MIN).words;
        assert_eq!(text_of(words), longest);
        assert_eq!(words[1], 24);
        heap::free(i64::try_from(words[0]).unwrap());
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn a_float_made_in_place_is_written_as_it_prints() {
        begin();
        for x in [
            0.0,
            -0.0,
            1.0,
            -2.25,
            0.1,
            1e-4,
            -0.000_123_456_789_012_345_67,
            1e15,
            9_999_999_999_999_998.0,
            123_456.789,
            1e16,
            9.9e-5,
            f64::MIN_POSITIVE,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert_eq!(text_of(super::float_header(x).words), crate::format::float(x), "{x:e}");
        }
        finish();
    }

    #[test]
    fn floats_of_every_size_are_written_as_they_print() {
        begin();
        // Floats from bits spread over every exponent, and ones made from a few decimal places,
        // as saved data has: each made in place has to be what `format::float` writes.
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200_000 {
            let bits = next();
            let spread = f64::from_bits(bits);
            let decimal = f64::from(u32::try_from(bits % 100_000_000).unwrap())
                / [1.0, 10.0, 100.0, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9]
                    [usize::try_from(bits >> 60).unwrap() % 10];
            for x in [spread, decimal, -decimal, decimal * 1e6, decimal / 1e3] {
                assert_eq!(text_of(super::float_header(x).words), crate::format::float(x), "{x:e}");
            }
        }
        finish();
    }

    #[test]
    fn a_string_of_23_bytes_or_fewer_is_held_in_its_header() {
        begin();
        let before = live();
        let short = header("twenty-three bytes long");
        assert_eq!(super::tag(&short), INLINE + 23);
        assert_eq!(inline_bytes(&short).as_deref(), Some(&b"twenty-three bytes long"[..]));
        assert_eq!(header(""), super::inline(&[]));
        assert_eq!(live(), before);
        let long = header("twenty-four bytes long!!");
        assert!(block_of(&long).is_some());
        assert_eq!(long[1], 24);
        assert_eq!(text_of(long), "twenty-four bytes long!!");
        assert_eq!(text_of(header("é and €")), "é and €");
        // Through the calls native code makes: a block of a header, and a header of a block.
        let block = super::block(short[0], short[1], short[2]);
        assert_eq!(super::of(block).words, short);
        let again = super::of(super::block(long[0], long[1], long[2]));
        assert_eq!(again.words, long);
        heap::free(i64::try_from(long[0]).unwrap());
        assert_eq!(live(), before);
        finish();
    }

    /// Freeing a struct or an enum lets go of the strings in its headers by its plan, with an
    /// inline one and a `None` holding nothing.
    #[test]
    fn freeing_a_block_lets_go_of_the_strings_in_its_headers() {
        let text = Element::Text;
        let shapes = vec![
            Rc::new(Element::Struct {
                name: Rc::from("Unit"),
                fields: vec![
                    (Rc::from("name"), text.clone()),
                    (Rc::from("hp"), Element::Int),
                    (Rc::from("pet"), Element::Maybe(Rc::new(text.clone()))),
                ],
            }),
            Rc::new(Element::Enum {
                name: Rc::from("Order"),
                cases: vec![
                    (Rc::from("Hold"), Vec::new()),
                    (Rc::from("Say"), vec![(Rc::from("what"), text)]),
                ],
                words: 4,
            }),
            Rc::new(Element::Str),
        ];
        let tables = Tables { shapes, str_shape: 2, text_headers: true, ..Tables::default() };
        start(tables, crate::Inputs::default());
        let before = live();
        let long = "a name too long for any header";
        let fill = |block: i64, words: &[u64]| {
            with(|state| {
                let fields = state.heap.block_mut(block.cast_unsigned())?.get_mut(heap::HEADER_WORDS..)?;
                fields.get_mut(..words.len())?.copy_from_slice(words);
                Some(())
            })
            .unwrap();
        };
        let (name, pet) = (header(long), header(long));
        let unit = heap::alloc(0);
        fill(unit, &[name[0], name[1], name[2], 7, pet[0], pet[1], pet[2]]);
        assert_eq!(live(), before + 3);
        heap::free(unit);
        assert_eq!(live(), before);
        let (short, none) = (header("ash"), [0_u64; 3]);
        let unit = heap::alloc(0);
        fill(unit, &[short[0], short[1], short[2], 7, none[0], none[1], none[2]]);
        heap::free(unit);
        assert_eq!(live(), before);
        let what = header(long);
        let order = heap::alloc(1);
        fill(order, &[1, what[0], what[1], what[2]]);
        assert_eq!(live(), before + 2);
        heap::free(order);
        assert_eq!(live(), before);
        let order = heap::alloc(1);
        fill(order, &[0, 0, 0, 0]);
        heap::free(order);
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn a_list_of_headers_is_joined_sorted_searched_compared_and_freed() {
        begin();
        let before = live();
        let names = ["medic", "a name too long for any header", "archer"];
        let items: Vec<Value> = names.iter().map(|name| Value::str(name)).collect();
        let temp = with(|state| Some(state.push(Value::list(items.clone())))).unwrap();
        let list = lists::from_value(temp, 0, 0, 0);
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(list).unwrap()).unpacked())).unwrap();
        assert_eq!(read, Value::list(items.clone()));
        let comma = with(|state| heap::text_address(state, ", "));
        let joined = strings::join(comma, list);
        let text = with(|state| Some(super::text_of(state, &[joined.cast_unsigned(), 0, 0]))).unwrap();
        assert_eq!(text, names.join(", "));
        let sorted = lists::sorted(list, lists::SORT_STRINGS);
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(sorted).unwrap()).unpacked())).unwrap();
        let mut ordered = names.to_vec();
        ordered.sort_unstable();
        assert_eq!(read, Value::list(ordered.iter().map(|name| Value::str(name)).collect()));
        // Sorted by keys that are strings too, here the list itself backwards.
        let keys: Vec<Value> = ["c", "b", "a"].iter().map(|key| Value::str(key)).collect();
        let keys = with(|state| Some(state.push(Value::list(keys)))).unwrap();
        let keys = lists::from_value(keys, 0, 0, 0);
        let by_keys = lists::sorted_by(list, keys, lists::SORT_STRINGS);
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(by_keys).unwrap()).unpacked())).unwrap();
        assert_eq!(read, Value::list(items.iter().rev().cloned().collect()));
        heap::free(keys);
        heap::free(by_keys);
        let wanted = with(|state| heap::text_address(state, "a name too long for any header"));
        assert_eq!(lists::contains(list, wanted), 1);
        let copy = lists::from_value(temp, 0, 0, 0);
        assert_eq!(heap::equal(list, copy), 1);
        let spaced = with(|state| heap::text_address(state, "a b"));
        let split = strings::split(spaced, 0, 0, 0);
        let element =
            with(|state| lists::element_of(state, state.heap.block(split.cast_unsigned())?)).unwrap();
        assert_eq!(*element, Element::Text);
        for done in [list, sorted, copy, split, spaced, joined, wanted, comma] {
            heap::free(done);
        }
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn an_optional_string_is_a_header_with_all_zero_for_none() {
        begin();
        let before = live();
        let items = vec![Value::None, Value::str("short"), Value::str("a string too long for any header")];
        let temp = with(|state| Some(state.push(Value::list(items.clone())))).unwrap();
        let list = lists::from_value(temp, 1, 0, 0);
        let words =
            with(|state| Some(lists::items(state.heap.block(list.cast_unsigned())?, 3).to_vec())).unwrap();
        assert_eq!(words.len(), 9);
        assert_eq!(words.get(..3), Some(&[0, 0, 0][..]));
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(list).unwrap()).unpacked())).unwrap();
        assert_eq!(read, Value::list(items.clone()));
        // Equal to a copy, and not to one with `None` where a string was.
        let copy = lists::from_value(temp, 1, 0, 0);
        assert_eq!(heap::equal(list, copy), 1);
        let other = vec![Value::None, Value::None, Value::str("a string too long for any header")];
        let other = with(|state| Some(state.push(Value::list(other)))).unwrap();
        let other = lists::from_value(other, 1, 0, 0);
        assert_eq!(heap::equal(list, other), 0);
        assert_eq!(heap::equal(other, list), 0);
        // `in` finds `None` and strings by their text.
        assert_eq!(lists::contains(list, 0), 1);
        let wanted = with(|state| heap::text_address(state, "a string too long for any header"));
        assert_eq!(lists::contains(list, wanted), 1);
        let missing = with(|state| heap::text_address(state, "not there"));
        assert_eq!(lists::contains(list, missing), 0);
        for done in [list, copy, other, wanted, missing] {
            heap::free(done);
        }
        assert_eq!(live(), before);
        finish();
    }

    /// Bytes packed eight to a word as a string's are, with `after` in the unused bytes past them,
    /// which a search must never read as text.
    fn packed(text: &str, after: u8) -> Vec<u64> {
        let mut bytes = text.as_bytes().to_vec();
        bytes.resize(text.len().div_ceil(8).max(1) * 8, after);
        bytes.chunks(8).map(|word| u64::from_le_bytes(word.try_into().unwrap())).collect()
    }

    #[test]
    fn looking_for_bytes_in_place_agrees_with_str_on_every_place_and_size() {
        let texts = [
            "",
            "a",
            "abcdefgh",
            "abcdefghi",
            "the quick brown fox jumps over the lazy dog",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
            "été à la plage, déjà vu, naïve — 日本語のテキスト",
        ];
        for text in texts {
            let words = packed(text, b'z');
            let places: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
            for (from, start) in places.iter().enumerate() {
                for end in places.iter().skip(from) {
                    let part = text.get(*start..*end).unwrap();
                    let found =
                        strings::bytes_contain((&words, text.len()), (&packed(part, b'q'), part.len()));
                    assert!(found, "`{part}` in `{text}`");
                }
            }
            for part in [
                "z",
                "zz",
                "q",
                "é!",
                "aab!",
                "abcdefgh!",
                "the quick brown fox!",
                "aaaaaaaaaaaaaaaaac",
                "h z",
            ] {
                let found = strings::bytes_contain((&words, text.len()), (&packed(part, b'q'), part.len()));
                assert_eq!(found, text.contains(part), "`{part}` in `{text}`");
            }
        }
    }

    #[test]
    fn looking_in_headers_makes_no_blocks_and_sees_inline_and_block_strings() {
        begin();
        let before = live();
        let long = "a string long enough that it can only be held in a block";
        let in_block = with(|state| Some(super::header(state, long))).unwrap();
        let found = |text: [u64; 3], part: [u64; 3]| {
            super::contains(text[0], text[1], text[2], part[0], part[1], part[2]) != 0
        };
        let holds = |part: &str| found(in_block, super::inline(part.as_bytes()));
        assert!(holds("only") && holds("") && holds("long enough"));
        assert!(!holds("nowhere"));
        let short = super::inline(b"short");
        assert!(!found(short, in_block), "a long string isn't in a short one");
        // Nothing past an inline string's bytes counts, like the tag in its last byte.
        assert!(!found(short, super::inline(b"\0")));
        assert_eq!(live(), before + 1);
        finish();
    }

    /// Joining a list of headers, inline and in blocks, wide characters too, gives what Rust's own
    /// join gives, with the characters counted, and one ASCII character is the block every such
    /// string shares.
    #[test]
    fn a_join_of_headers_gives_what_rusts_own_gives() {
        begin();
        super::super::aot::ascii();
        let long = "a line long enough to need a block of its own";
        let parts =
            ["", "a", "\u{e9}t\u{e9}", long, "twenty-three bytes long", "\u{65e5}\u{1f98a} and more of it"];
        for separator in ["", "\n", ", ", "\u{2014}"] {
            let between = with(|state| heap::text_address(state, separator));
            for count in 0..=parts.len() {
                let chosen: Vec<&str> = parts.iter().cycle().skip(count).take(count).copied().collect();
                let list = lists::new(0, i64::try_from(chosen.len()).unwrap());
                with(|state| {
                    for (at, part) in chosen.iter().enumerate() {
                        let words = super::header(state, part);
                        let block = state.heap.block_mut(list.cast_unsigned()).unwrap();
                        let start = lists::LIST_HEADER_WORDS + 3 * at;
                        block.get_mut(start..start + 3).unwrap().copy_from_slice(&words);
                    }
                });
                let joined = strings::join(between, list);
                let whole = chosen.join(separator);
                with(|state| {
                    let block = state.heap.block(joined.cast_unsigned()).unwrap();
                    assert_eq!(heap::block_text(block), whole, "{chosen:?} by {separator:?}");
                    assert_eq!(block.get(3).copied(), Some(u64::try_from(whole.chars().count()).unwrap()));
                });
                heap::free(list);
                // Let go of as native code does, which leaves the block a lone ASCII character shares.
                with(|state| heap::release(state, joined.cast_unsigned()));
            }
            with(|state| heap::release(state, between.cast_unsigned()));
        }
        let one = lists::new(0, 1);
        with(|state| {
            let words = super::header(state, "x");
            let block = state.heap.block_mut(one.cast_unsigned()).unwrap();
            block
                .get_mut(lists::LIST_HEADER_WORDS..lists::LIST_HEADER_WORDS + 3)
                .unwrap()
                .copy_from_slice(&words);
        });
        let empty = with(|state| heap::text_address(state, ""));
        let x = with(|state| heap::text_address(state, "x"));
        assert_eq!(strings::join(empty, one), x);
        finish();
    }

    /// A list of strings with no items, and a dict of strings with no entries used, hold nothing,
    /// so freeing them walks nothing: one made empty, one emptied after holding strings in blocks
    /// of their own, and a dict whose only key was taken out again all free what they held once,
    /// and leave nothing behind.
    #[test]
    fn empty_lists_and_dicts_of_strings_free_whole_without_a_walk() {
        use super::super::dicts;
        begin();
        let before = live();
        heap::free(lists::new(0, 0));
        let emptied = lists::new(0, 2);
        with(|state| {
            for at in 0..2 {
                let header = super::header(state, "a string too long to be held in a header");
                let block = state.heap.block_mut(emptied.cast_unsigned()).unwrap();
                let start = lists::LIST_HEADER_WORDS + 3 * at;
                block.get_mut(start..start + 3).unwrap().copy_from_slice(&header);
            }
        });
        assert_eq!(live(), before + 3);
        let emptied = lists::truncate(emptied, 0);
        assert_eq!(live(), before + 1, "the strings went as it was emptied");
        heap::free(emptied);
        assert_eq!(live(), before);
        let shape = i64::try_from(dicts::shape(0, dicts::Word::Str, dicts::Word::Int).unwrap()).unwrap();
        heap::free(dicts::new(shape, 0));
        let long = header("a key much longer than any header");
        let mut dict = dicts::set_header(dicts::new(shape, 0), long[0], long[1], long[2], 1);
        dict = dicts::pop_header(dict, long[0], long[1], long[2], 0, 0);
        assert_eq!(dicts::popped(), 1);
        heap::free(dict);
        assert_eq!(live(), before);
        finish();
    }

    /// `+=`, `find`, `startswith` and `endswith` given their pieces as headers, inline or holding a
    /// block, do what Rust's own do, characters counted, and `text += text` through its own block's
    /// header reads what it had before it grew.
    #[test]
    fn text_calls_given_headers_do_what_rusts_own_do() {
        begin();
        let before = live();
        let long = "a piece long enough to need a block of its own";
        let mut text = with(|state| heap::text_address(state, "ab"));
        let mut expected = String::from("ab");
        for piece in ["", "c", "\u{e9}t\u{e9}", long, "twenty-three bytes long", "\u{65e5}\u{1f98a}"] {
            let piece_header = header(piece);
            let [h0, h1, h2] = piece_header;
            text = super::append_header(text, h0, h1, h2);
            expected.push_str(piece);
            // The piece is only read, so a block it holds is still the test's to let go of.
            if let Some(address) = block_of(&piece_header) {
                heap::free_text(address.cast_signed());
            }
            with(|state| {
                let block = state.heap.block(text.cast_unsigned()).unwrap();
                assert_eq!(heap::block_text(block), expected);
                assert_eq!(block.get(3).copied(), Some(u64::try_from(expected.chars().count()).unwrap()));
            });
        }
        // Its own block's header: what it had, added once.
        let length = u64::try_from(expected.len()).unwrap();
        text = super::append_header(text, text.cast_unsigned(), length, 0);
        expected = expected.repeat(2);
        with(|state| assert_eq!(heap::block_text(state.heap.block(text.cast_unsigned()).unwrap()), expected));
        let whole = header("caf\u{e9} au lait, \u{e9}t\u{e9} long enough for a block");
        let [t0, t1, t2] = whole;
        for (part, at, starts, ends) in [
            ("caf", 0, true, false),
            ("\u{e9}t\u{e9}", 14, false, false),
            ("for a block", 30, false, true),
            ("absent", -1, false, false),
            ("", 0, true, true),
        ] {
            let [p0, p1, p2] = header(part);
            assert_eq!(super::find_header(t0, t1, t2, p0, p1, p2), at, "{part:?}");
            assert_eq!(super::starts_with(t0, t1, t2, p0, p1, p2), i8::from(starts), "{part:?}");
            assert_eq!(super::ends_with(t0, t1, t2, p0, p1, p2), i8::from(ends), "{part:?}");
        }
        // An inline text, its bytes in the header itself, with its tag after them never matched.
        let [i0, i1, i2] = header("Host: \u{e9}x.com");
        for (part, at) in
            [(": ", 4), ("\u{e9}x", 6), ("x.com", 7), ("com\u{80}", -1), ("Host: \u{e9}x.com!", -1)]
        {
            let [p0, p1, p2] = header(part);
            assert_eq!(super::find_header(i0, i1, i2, p0, p1, p2), at, "{part:?}");
        }
        heap::free_text(text);
        if let Some(address) = block_of(&whole) {
            heap::free_text(address.cast_signed());
        }
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn a_dict_of_strings_finds_and_sets_a_key_given_as_its_header_without_making_a_block() {
        use super::super::dicts;
        begin();
        let before = live();
        let shape = dicts::shape(0, dicts::Word::Str, dicts::Word::Int).unwrap();
        let mut dict = dicts::new(i64::try_from(shape).unwrap(), 4);
        let read = |dict: i64, key: [u64; 3]| {
            let found = dicts::find_header(dict, key[0], key[1], key[2]);
            (found != 0).then(|| {
                with(|state| {
                    let block = state.heap.block(dict.cast_unsigned())?;
                    let at = (usize::try_from(found).ok()? - block.as_ptr().addr()) / 8;
                    block.get(at).copied()
                })
            })
        };
        let short = header("fox");
        dict = dicts::set_header(dict, short[0], short[1], short[2], 1);
        let allocations = with(|state| state.heap.counts.allocations);
        assert_eq!(read(dict, header("fox")), Some(Some(1)));
        assert_eq!(read(dict, header("cat")), None);
        assert_eq!(with(|state| state.heap.counts.allocations), allocations);
        dict = dicts::set_header(dict, short[0], short[1], short[2], 2);
        assert_eq!(read(dict, short), Some(Some(2)));
        let long = header("a key much longer than any header");
        dict = dicts::set_header(dict, long[0], long[1], long[2], 3);
        // The dict holds the key's block now, so the header still reads it.
        assert_eq!(read(dict, long), Some(Some(3)));
        dict = dicts::pop_header(dict, short[0], short[1], short[2], 0, 0);
        assert_eq!(dicts::popped(), 2);
        assert_eq!(read(dict, short), None);
        heap::free(dict);
        assert_eq!(live(), before);
        finish();
    }
}
