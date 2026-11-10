//! Dicts and sets, in the runtime's blocks: lookups here, and anything that grows or copies one
//! in the runtime. A key's hash is worked out the way the runtime does it.

use std::fmt::Write as _;

use fennec_ir::{LocalId, Operand};
use fennec_layout::Ty;
use fennec_rt::embed::dicts::{DICT_HEADER_WORDS, ENTRY_WORDS, LONGEST_PROBE, Word};
use fennec_rt::embed::layout::{TEXT, TEXT_BYTES, TEXT_CHARS};
use fennec_syntax::{BinOp, Span};

use crate::emit::Emit;
use crate::kinds::Kind;

const MIX: i64 = 0x9E37_79B9_7F4A_7C15_u64.cast_signed();
/// Where a dict's count of keys, entries used and slots less one are.
const KEYS: u32 = 16;
const USED: u32 = 24;
const MASK: u32 = 32;
/// Where a dict says how its keys are hashed: 0 the fast way, 1 the strong way.
const HOW_HASHED: u32 = 40;
/// Where an entry's key is.
const ENTRY_KEY: u32 = 8;
/// The hash a removed entry has.
const REMOVED: i64 = -1;

/// What a find helper does for a dict hashed the strong way, for a one-word key.
const STRONG_WORD: &str = "  %strong_hash = call i64 @fen_strong_word(i64 %key)\n";

/// The same for a key of 23 bytes or fewer, in a helper that has `%bytes`.
const STRONG_BYTES_OF_BYTES: &str = "  %strong_hash = call i64 @fen_strong_bytes(ptr %bytes, i64 %length)\n";

/// The same for a key given as its bytes and their length.
const STRONG_BYTES: &str = "  %strong_hash = call i64 @fen_strong_bytes(ptr %key, i64 %length)\n";

/// Loads the hash seed, which is set before the program starts and never changes.
const SEEDED_LENGTH: &str =
    "  %seed = load i64, ptr @fennec_hash_seed, !invariant.load !1\n  %seeded = xor i64 %length, %seed\n";

fn header_bytes() -> i64 {
    i64::try_from(DICT_HEADER_WORDS * 8).unwrap_or(40)
}

/// How many words a dict's entries take, by what its keys are: a string held as its header
/// takes three, any other key one.
fn entry_words(key: Word) -> usize {
    if key == Word::Text { 5 } else { ENTRY_WORDS }
}

/// Where an entry's value is, after its hash and key.
fn entry_value(key: Word) -> u32 {
    u32::try_from(entry_words(key) * 8 - 8).unwrap_or(16)
}

/// The start of a function that looks up a key: gives back its entry's address, or zero.
fn find_start(name: &str, key_type: &str, strong: &str) -> String {
    find_start_wide(name, key_type, ENTRY_WORDS, strong)
}

/// [`find_start`] for entries `width` words wide.
fn find_start_wide(name: &str, key_type: &str, width: usize, strong: &str) -> String {
    let header = header_bytes();
    format!(
        "define internal ptr @{name}(ptr %d, {key_type} %key, i64 %given) #0 {{\nentry:\n  %how_at = getelementptr inbounds i8, ptr %d, i64 {HOW_HASHED}\n  %how = load i64, ptr %how_at\n  %is_strong = icmp ne i64 %how, 0\n  br i1 %is_strong, label %rehash, label %probe, !prof !0\nrehash:\n{strong}  br label %probe\nprobe:\n  %hash = phi i64 [ %given, %entry ], [ %strong_hash, %rehash ]\n  %mask_at = getelementptr inbounds i8, ptr %d, i64 {MASK}\n  %mask = load i64, ptr %mask_at\n  %slots = getelementptr inbounds i8, ptr %d, i64 {header}\n  %count = add i64 %mask, 1\n  %first = and i64 %hash, %mask\n  %entries = getelementptr inbounds i64, ptr %slots, i64 %count\n  br label %head\nhead:\n  %at = phi i64 [ %first, %probe ], [ %after, %next ]\n  %slot = getelementptr inbounds i64, ptr %slots, i64 %at\n  %taken = load i64, ptr %slot\n  %empty = icmp eq i64 %taken, 0\n  br i1 %empty, label %none, label %check\nnone:\n  ret ptr null\ncheck:\n  %number = add i64 %taken, -1\n  %entry_at = getelementptr inbounds [{width} x i64], ptr %entries, i64 %number\n  %theirs = load i64, ptr %entry_at\n  %same_hash = icmp eq i64 %theirs, %hash\n  br i1 %same_hash, label %compare, label %next\nnext:\n  %stepped = add i64 %at, 1\n  %after = and i64 %stepped, %mask\n  br label %head\nfound:\n  ret ptr %entry_at\ncompare:\n  %other_at = getelementptr inbounds i8, ptr %entry_at, i64 {ENTRY_KEY}\n"
    )
}

impl Emit<'_, '_, '_> {
    fn word_kind(&self, word: Word) -> Option<Kind> {
        // A dict holds a string as its block, where strings are otherwise headers.
        if word == Word::Str && self.headers() {
            return Some(Kind::TextBlock);
        }
        Kind::from_ty(self.module.layouts, word_ty(word))
    }

    /// The number of the shape of a dict with these keys and values, which the runtime sets.
    pub(crate) fn dict_shape(&mut self, key: Word, value: Word) -> String {
        let symbol = format!("d_{key:?}_{value:?}");
        self.module.shared.dicts.insert(symbol.clone(), (key, value));
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        let shape = self.temp();
        self.line(&format!("{shape} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        shape
    }

    /// A new empty dict with room for `room` keys.
    fn new_dict(&mut self, key: Word, value: Word, room: &str) -> String {
        let shape = self.dict_shape(key, value);
        self.declare("declare ptr @fennec_dict_new(i64, i64) nounwind");
        let dict = self.temp();
        self.line(&format!("{dict} = call ptr @fennec_dict_new(i64 {shape}, i64 {room})"));
        dict
    }

    /// A key's hash. `key` is a number, or the address of a string or a plain struct.
    fn hash(&mut self, word: Word, key: &str) -> String {
        let mixed = match word {
            Word::Str => {
                self.string_hash_helper();
                let held = self.temp();
                self.line(&format!("{held} = call i64 @fen_text_hash(ptr {key})"));
                held
            }
            Word::Plain(id) => {
                let words = self.module.layouts.get(id).map_or(0, |layout| layout.size / 8);
                let (seeded, started) = (self.temp(), self.temp());
                let seed = self.hash_seed();
                self.line(&format!("{seeded} = xor i64 {words}, {seed}"));
                self.line(&format!("{started} = mul i64 {seeded}, {MIX}"));
                let mut hash = started;
                for at in 0..words {
                    let (place, word_held, turned, mixed, next) =
                        (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
                    self.line(&format!("{place} = getelementptr inbounds i64, ptr {key}, i64 {at}"));
                    self.line(&format!("{word_held} = load i64, ptr {place}"));
                    self.declare("declare i64 @llvm.fshl.i64(i64, i64, i64)");
                    self.line(&format!("{turned} = call i64 @llvm.fshl.i64(i64 {hash}, i64 {hash}, i64 23)"));
                    self.line(&format!("{mixed} = xor i64 {turned}, {word_held}"));
                    self.line(&format!("{next} = mul i64 {mixed}, {MIX}"));
                    hash = next;
                }
                hash
            }
            _ => {
                let (seeded, held) = (self.temp(), self.temp());
                let seed = self.hash_seed();
                self.line(&format!("{seeded} = xor i64 {key}, {seed}"));
                self.line(&format!("{held} = mul i64 {seeded}, {MIX}"));
                held
            }
        };
        let (high, folded, done) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{high} = lshr i64 {mixed}, 29"));
        self.line(&format!("{folded} = xor i64 {mixed}, {high}"));
        self.line(&format!("{done} = lshr i64 {folded}, 1"));
        done
    }

    /// This run's hash seed, as a value.
    fn hash_seed(&mut self) -> String {
        self.declare("@fennec_hash_seed = external dso_local global i64");
        let seed = self.temp();
        self.line(&format!("{seed} = load i64, ptr @fennec_hash_seed, !invariant.load !1"));
        seed
    }

    /// Defines what a find helper calls to hash a key for a dict hashed the strong way: the same
    /// words as `strong_hash` in the runtime, one `fennec_dict_strong_step` each.
    fn strong_hash_helpers(&mut self) {
        self.declare("declare i64 @fennec_dict_strong_step(i64, i64) nounwind");
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        self.module.add_helper(
            "fen_strong_word",
            "define internal i64 @fen_strong_word(i64 %key) #0 {\nentry:\n  %count = call i64 @fennec_dict_strong_step(i64 0, i64 1)\n  %mixed = call i64 @fennec_dict_strong_step(i64 %count, i64 %key)\n  %hash = lshr i64 %mixed, 1\n  ret i64 %hash\n}\n\n".to_owned(),
        );
        self.module.add_helper(
            "fen_strong_words",
            "define internal i64 @fen_strong_words(ptr %key, i64 %count) #0 {\nentry:\n  %start = call i64 @fennec_dict_strong_step(i64 0, i64 %count)\n  br label %head\nhead:\n  %mixed = phi i64 [ %start, %entry ], [ %after, %body ]\n  %at = phi i64 [ 0, %entry ], [ %next, %body ]\n  %more = icmp ult i64 %at, %count\n  br i1 %more, label %body, label %done\nbody:\n  %place = getelementptr inbounds i64, ptr %key, i64 %at\n  %word = load i64, ptr %place\n  %after = call i64 @fennec_dict_strong_step(i64 %mixed, i64 %word)\n  %next = add i64 %at, 1\n  br label %head\ndone:\n  %hash = lshr i64 %mixed, 1\n  ret i64 %hash\n}\n\n".to_owned(),
        );
        self.module.add_helper(
            "fen_strong_bytes",
            "define internal i64 @fen_strong_bytes(ptr %bytes, i64 %length) #0 {\nentry:\n  %part = alloca i64\n  %start = call i64 @fennec_dict_strong_step(i64 0, i64 %length)\n  %whole = lshr i64 %length, 3\n  br label %head\nhead:\n  %mixed = phi i64 [ %start, %entry ], [ %after, %body ]\n  %at = phi i64 [ 0, %entry ], [ %next, %body ]\n  %more = icmp ult i64 %at, %whole\n  br i1 %more, label %body, label %tail\nbody:\n  %place = getelementptr inbounds i64, ptr %bytes, i64 %at\n  %word = load i64, ptr %place, align 1\n  %after = call i64 @fennec_dict_strong_step(i64 %mixed, i64 %word)\n  %next = add i64 %at, 1\n  br label %head\ntail:\n  %rest = and i64 %length, 7\n  %some = icmp ne i64 %rest, 0\n  br i1 %some, label %last, label %done\nlast:\n  store i64 0, ptr %part\n  %from = getelementptr inbounds i64, ptr %bytes, i64 %whole\n  call void @llvm.memcpy.p0.p0.i64(ptr %part, ptr %from, i64 %rest, i1 false)\n  %last_word = load i64, ptr %part\n  %last_mixed = call i64 @fennec_dict_strong_step(i64 %mixed, i64 %last_word)\n  %last_hash = lshr i64 %last_mixed, 1\n  ret i64 %last_hash\ndone:\n  %hash = lshr i64 %mixed, 1\n  ret i64 %hash\n}\n\n".to_owned(),
        );
    }

    fn string_hash_helper(&mut self) {
        self.strong_hash_helpers();
        self.declare("@fennec_hash_seed = external dso_local global i64");
        self.module.add_helper(
            "fen_text_hash",
            format!(
                "define internal i64 @fen_text_hash(ptr %text) #0 {{\nentry:\n  %length_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_BYTES}\n  %length = load i64, ptr %length_at\n{SEEDED_LENGTH}  %start = mul i64 %seeded, {MIX}\n  %rounded = add i64 %length, 7\n  %words = lshr i64 %rounded, 3\n  %first = getelementptr inbounds i8, ptr %text, i64 {TEXT}\n  br label %head\nhead:\n  %hash = phi i64 [ %start, %entry ], [ %mixed, %body ]\n  %at = phi i64 [ 0, %entry ], [ %next, %body ]\n  %more = icmp ult i64 %at, %words\n  br i1 %more, label %body, label %done\nbody:\n  %place = getelementptr inbounds i64, ptr %first, i64 %at\n  %word = load i64, ptr %place\n  %turned = call i64 @llvm.fshl.i64(i64 %hash, i64 %hash, i64 23)\n  %xored = xor i64 %turned, %word\n  %mixed = mul i64 %xored, {MIX}\n  %next = add i64 %at, 1\n  br label %head\ndone:\n  ret i64 %hash\n}}\n\n"
            ),
        );
        self.declare("declare i64 @llvm.fshl.i64(i64, i64, i64)");
    }

    /// The address of a key's entry, or zero.
    fn find(&mut self, dict: &str, word: Word, key: &str) -> String {
        self.strong_hash_helpers();
        if matches!(word, Word::Float | Word::F32) {
            return self.find_float_key(dict, key);
        }
        if let Word::Plain(id) = word
            && self.key_by_value(id)
        {
            return self.find_by_value(dict, id, key);
        }
        // A key held as a header is found by its bytes here, as a slice's are: its inline bytes
        // are zero after its length, as a block's text is, so they hash the same.
        if word == Word::Text {
            let (bytes, length) = (self.header_bytes(key), self.header_byte_length(key));
            // An inline key's header is copied whole where `header_bytes` puts it.
            return self.find_text_key(dict, (&bytes, KeyBytes::Header), &length);
        }
        if word == Word::Str && self.headers() {
            self.string_slice_helpers();
            let (bytes, length) = (self.header_bytes(key), self.header_byte_length(key));
            let (mixed, high, folded, hash, entry) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{mixed} = call i64 @fen_slice_hash(ptr {bytes}, i64 {length})"));
            self.line(&format!("{high} = lshr i64 {mixed}, 29"));
            self.line(&format!("{folded} = xor i64 {mixed}, {high}"));
            self.line(&format!("{hash} = lshr i64 {folded}, 1"));
            self.line(&format!(
                "{entry} = call ptr @fen_find_slice(ptr {dict}, ptr {bytes}, i64 {length}, i64 {hash})"
            ));
            return entry;
        }
        let hash = self.hash(word, key);
        let (name, key_type) = match word {
            Word::Str => {
                self.declare("declare i8 @fennec_box_equal(ptr, ptr) nounwind");
                let strong = format!(
                    "  %length_at = getelementptr inbounds i8, ptr %key, i64 {TEXT_BYTES}\n  %length = load i64, ptr %length_at\n  %text_at = getelementptr inbounds i8, ptr %key, i64 {TEXT}\n  %strong_hash = call i64 @fen_strong_bytes(ptr %text_at, i64 %length)\n"
                );
                let mut text = find_start("fen_find_text", "ptr", &strong);
                text += "  %other = load ptr, ptr %other_at\n  %same = icmp eq ptr %other, %key\n  br i1 %same, label %found, label %text\ntext:\n  %equal = call i8 @fennec_box_equal(ptr %other, ptr %key)\n  %is = icmp ne i8 %equal, 0\n  br i1 %is, label %found, label %next\n}\n\n";
                self.module.add_helper("fen_find_text", text);
                ("fen_find_text".to_owned(), "ptr")
            }
            Word::Plain(id) => {
                let words = self.module.layouts.get(id).map_or(0, |layout| layout.size / 8);
                let name = format!("fen_find_words{words}");
                let strong = format!("  %strong_hash = call i64 @fen_strong_words(ptr %key, i64 {words})\n");
                let mut text = find_start(&name, "ptr", &strong);
                text += "  %other = load ptr, ptr %other_at\n  br label %w0\n";
                for at in 0..words {
                    let after = if at + 1 == words { "found".to_owned() } else { format!("w{}", at + 1) };
                    let bytes = 16 + at * 8;
                    let _ = write!(
                        text,
                        "w{at}:\n  %a{at}_at = getelementptr inbounds i8, ptr %other, i64 {bytes}\n  %b{at}_at = getelementptr inbounds i64, ptr %key, i64 {at}\n  %a{at} = load i64, ptr %a{at}_at\n  %b{at} = load i64, ptr %b{at}_at\n  %s{at} = icmp eq i64 %a{at}, %b{at}\n  br i1 %s{at}, label %{after}, label %next\n"
                    );
                }
                text += "}\n\n";
                self.module.add_helper(&name, text);
                (name, "ptr")
            }
            _ => {
                let mut text = find_start("fen_find_int", "i64", STRONG_WORD);
                text += "  %other = load i64, ptr %other_at\n  %same = icmp eq i64 %other, %key\n  br i1 %same, label %found, label %next\n}\n\n";
                self.module.add_helper("fen_find_int", text);
                ("fen_find_int".to_owned(), "i64")
            }
        };
        let entry = self.temp();
        self.line(&format!("{entry} = call ptr @{name}(ptr {dict}, {key_type} {key}, i64 {hash})"));
        entry
    }

    /// Whether a plain key is hashed by what it holds, which the runtime does: strings and lists,
    /// or floats, whose bits don't say what's equal.
    fn key_by_value(&self, id: u32) -> bool {
        self.module.layouts.holds_counted(id) || self.holds_a_float(id)
    }

    /// A plain key as a block of its own, with owners of what it holds.
    fn owned_key_block(&mut self, id: u32, key: &str) -> String {
        let block = self.key_block(id, key).unwrap_or_default();
        let kind = Kind::from_ty(self.module.layouts, Ty::Struct(id)).unwrap_or(Kind::Struct(id));
        let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
        self.retain_inline(&kind, &fields);
        block
    }

    /// The entry of a plain key that's hashed by what it holds.
    fn find_by_value(&mut self, dict: &str, id: u32, key: &str) -> String {
        self.declare("declare ptr @fennec_dict_find_by_value(ptr, ptr) nounwind");
        let block = self.owned_key_block(id, key);
        let entry = self.temp();
        self.line(&format!("{entry} = call ptr @fennec_dict_find_by_value(ptr {dict}, ptr {block})"));
        self.release(&block);
        entry
    }

    /// The entry of a float key, given as its bits. Floats have one equality, with `-0.0` the same
    /// as `0.0` and every NaN the same, so they're hashed and compared as one form of each, while
    /// the entry keeps the key as it was written.
    fn find_float_key(&mut self, dict: &str, key: &str) -> String {
        let nan = f64::NAN.to_bits().cast_signed();
        self.module.add_helper(
            "fen_float_key",
            format!(
                "define internal i64 @fen_float_key(i64 %bits) #0 {{\nentry:\n  %value = bitcast i64 %bits to double\n  %zero = fcmp oeq double %value, 0x0000000000000000\n  %nan = fcmp uno double %value, %value\n  %plain = select i1 %zero, i64 0, i64 %bits\n  %key = select i1 %nan, i64 {nan}, i64 %plain\n  ret i64 %key\n}}\n\n"
            ),
        );
        let mut text = find_start("fen_find_float", "i64", STRONG_WORD);
        text += "  %other = load i64, ptr %other_at\n  %other_key = call i64 @fen_float_key(i64 %other)\n  %same = icmp eq i64 %other_key, %key\n  br i1 %same, label %found, label %next\n}\n\n";
        self.module.add_helper("fen_find_float", text);
        let wanted = self.temp();
        self.line(&format!("{wanted} = call i64 @fen_float_key(i64 {key})"));
        let hash = self.hash(Word::Int, &wanted);
        let entry = self.temp();
        self.line(&format!("{entry} = call ptr @fen_find_float(ptr {dict}, i64 {wanted}, i64 {hash})"));
        entry
    }

    /// The entry of the string key with these bytes in a dict whose keys are held as headers,
    /// or null: hashed as a block's text is, and compared by bytes, inline keys where they lie.
    fn find_text_key(&mut self, dict: &str, (bytes, from): (&str, KeyBytes), length: &str) -> String {
        let short_find = self.find_short_key_helper(from);
        let (short, quick, slow, joined) =
            (self.temp(), self.label("short_key"), self.label("long_key"), self.label("key_found"));
        self.line(&format!("{short} = icmp ule i64 {length}, 23"));
        self.line(&format!("br i1 {short}, label %{quick}, label %{slow}"));
        self.start_block(&quick);
        let found_short = self.temp();
        self.line(&format!("{found_short} = call ptr @{short_find}(ptr {dict}, ptr {bytes}, i64 {length})"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&slow);
        let found_long = self.find_long_text_key(dict, bytes, length);
        let long_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let entry = self.temp();
        self.line(&format!("{entry} = phi ptr [ {found_short}, %{quick} ], [ {found_long}, %{long_end} ]"));
        entry
    }

    /// [`Self::find_text_key`] for a key of any length, compared by its bytes.
    fn find_long_text_key(&mut self, dict: &str, bytes: &str, length: &str) -> String {
        self.string_slice_helpers();
        let mut find = find_start_wide("fen_find_header_key", "ptr", entry_words(Word::Text), STRONG_BYTES)
            .replace("(ptr %d, ptr %key, i64 %given)", "(ptr %d, ptr %key, i64 %length, i64 %given)");
        find += &format!(
            "  %last_at = getelementptr inbounds i8, ptr %entry_at, i64 24\n  %last = load i64, ptr %last_at\n  %inline = icmp slt i64 %last, 0\n  br i1 %inline, label %short, label %long\nshort:\n  %tag = lshr i64 %last, 56\n  %short_length = and i64 %tag, 127\n  %same_short = icmp eq i64 %short_length, %length\n  br i1 %same_short, label %short_bytes, label %next\nshort_bytes:\n  %short_order = call i32 @memcmp(ptr %other_at, ptr %key, i64 %length)\n  %short_equal = icmp eq i32 %short_order, 0\n  br i1 %short_equal, label %found, label %next\nlong:\n  %other = load ptr, ptr %other_at\n  %long_length_at = getelementptr inbounds i8, ptr %other, i64 {TEXT_BYTES}\n  %long_length = load i64, ptr %long_length_at\n  %same_long = icmp eq i64 %long_length, %length\n  br i1 %same_long, label %long_bytes, label %next\nlong_bytes:\n  %their_text = getelementptr inbounds i8, ptr %other, i64 {TEXT}\n  %long_order = call i32 @memcmp(ptr %their_text, ptr %key, i64 %length)\n  %long_equal = icmp eq i32 %long_order, 0\n  br i1 %long_equal, label %found, label %next\n}}\n\n"
        );
        self.module.add_helper("fen_find_header_key", find);
        let (mixed, high, folded, hash, entry) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{mixed} = call i64 @fen_slice_hash(ptr {bytes}, i64 {length})"));
        self.line(&format!("{high} = lshr i64 {mixed}, 29"));
        self.line(&format!("{folded} = xor i64 {mixed}, {high}"));
        self.line(&format!("{hash} = lshr i64 {folded}, 1"));
        self.line(&format!(
            "{entry} = call ptr @fen_find_header_key(ptr {dict}, ptr {bytes}, i64 {length}, i64 {hash})"
        ));
        entry
    }

    /// A key as `find` takes it: a word for a number, or an address.
    fn key_value(&mut self, operand: &Operand, word: Word) -> Result<String, String> {
        let (value, kind) = self.operand(operand)?;
        if kind == Kind::Bool {
            let wide = self.temp();
            self.line(&format!("{wide} = zext i1 {value} to i64"));
            return Ok(wide);
        }
        if matches!(word, Word::Float | Word::F32) {
            let bits = self.temp();
            self.line(&format!("{bits} = bitcast double {value} to i64"));
            return Ok(bits);
        }
        Ok(value)
    }

    /// A value as the word a dict keeps, handing over an owner if it's a block.
    fn dict_word(&mut self, kind: &Kind, value: &str, retain: bool) -> String {
        let word = self.temp();
        match kind {
            Kind::Float | Kind::F32 => self.line(&format!("{word} = bitcast double {value} to i64")),
            Kind::Bool => self.line(&format!("{word} = zext i1 {value} to i64")),
            // A header becomes a block of its own, the dict's.
            Kind::Str | Kind::TextBlock if self.headers() => {
                let block = self.owned_text_block(value);
                if !retain {
                    self.release_text(value);
                }
                self.line(&format!("{word} = ptrtoint ptr {block} to i64"));
            }
            Kind::Str | Kind::Boxed(_) | Kind::List(_) | Kind::Dict(..) | Kind::Bytes => {
                if retain {
                    self.retain(value);
                }
                self.line(&format!("{word} = ptrtoint ptr {value} to i64"));
            }
            Kind::Maybe(_) => {
                if retain {
                    self.retain_value(value, kind);
                }
                self.line(&format!("{word} = ptrtoint ptr {value} to i64"));
            }
            // A plain struct or enum as a value: a block of its own, with owners of what it holds.
            Kind::Struct(id) | Kind::Enum(id) => {
                if let Ok(block) = self.key_block(*id, value) {
                    let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                    self.retain_inline(kind, &fields);
                    self.line(&format!("{word} = ptrtoint ptr {block} to i64"));
                }
            }
            Kind::Unit => return "0".to_owned(),
            _ => return value.to_owned(),
        }
        word
    }

    /// A plain struct key as a block of its own, owned once, as a dict keeps it.
    fn key_block(&mut self, id: u32, key: &str) -> Result<String, String> {
        let words = self.module.layouts.get(id).map_or(0, |layout| layout.size / 8);
        let block = self.new_block(id)?;
        for at in 0..words {
            let (from, to, held) = (self.temp(), self.temp(), self.temp());
            self.line(&format!("{from} = getelementptr inbounds i64, ptr {key}, i64 {at}"));
            self.line(&format!("{to} = getelementptr inbounds i8, ptr {block}, i64 {}", 16 + at * 8));
            self.line(&format!("{held} = load i64, ptr {from}"));
            self.line(&format!("store i64 {held}, ptr {to}"));
        }
        Ok(block)
    }

    /// A key as the word the runtime takes, an owner handed over if it's a block.
    fn key_word(&mut self, word: Word, key: &str) -> Result<String, String> {
        Ok(match word {
            Word::Plain(id) => {
                let block = if self.key_by_value(id) {
                    self.owned_key_block(id, key)
                } else {
                    self.key_block(id, key)?
                };
                let held = self.temp();
                self.line(&format!("{held} = ptrtoint ptr {block} to i64"));
                held
            }
            Word::Str => self.dict_word(&Kind::Str, key, true),
            _ => key.to_owned(),
        })
    }

    pub(crate) fn make_dict(&mut self, dest: LocalId, pairs: &[(Operand, Operand)]) -> Result<(), String> {
        let Kind::Dict(key, value) = self.kind_of(dest)?.clone() else {
            return Err("it makes a dict this backend doesn't take yet".to_owned());
        };
        let dict = self.new_dict(key, value, &pairs.len().to_string());
        let held = self.temp_slot(&dict);
        for (written_key, written_value) in pairs {
            self.dict_store(&held, (key, value), written_key, Some(written_value))?;
        }
        let dict = self.temp();
        self.line(&format!("{dict} = load ptr, ptr {held}"));
        self.assign(dest, &dict)
    }

    pub(crate) fn make_set(&mut self, dest: LocalId, items: &[Operand]) -> Result<(), String> {
        let Kind::Dict(key, value) = self.kind_of(dest)?.clone() else {
            return Err("it makes a set this backend doesn't take yet".to_owned());
        };
        let dict = self.new_dict(key, value, &items.len().to_string());
        let held = self.temp_slot(&dict);
        for item in items {
            self.dict_store(&held, (key, value), item, None)?;
        }
        let dict = self.temp();
        self.line(&format!("{dict} = load ptr, ptr {held}"));
        self.assign(dest, &dict)
    }

    /// `set()`: a new set with nothing in it.
    pub(crate) fn empty_set(&mut self, dest: LocalId) -> Result<(), String> {
        let Kind::Dict(key, value) = self.kind_of(dest)?.clone() else {
            return Err("it makes a set this backend doesn't take yet".to_owned());
        };
        let dict = self.new_dict(key, value, "0");
        self.assign(dest, &dict)
    }

    /// `set(list)`: a new set of a list's items, the first of equal ones staying.
    pub(crate) fn set_of_list(&mut self, dest: LocalId, list: &str, held: &Kind) -> Result<(), String> {
        let Kind::Dict(key, value) = self.kind_of(dest)?.clone() else {
            return Err("it makes a set this backend doesn't take yet".to_owned());
        };
        let length = self.list_length(list);
        let dict = self.new_dict(key, value, &length);
        let slot = self.temp_slot(&dict);
        let (at, end) = self.loop_begin(&length);
        let item = self.item_value(list, held, &at);
        let item = if *held == Kind::Bool {
            let wide = self.temp();
            self.line(&format!("{wide} = zext i1 {item} to i64"));
            wide
        } else {
            item
        };
        self.dict_store_word(&slot, (key, value), &item, "", None)?;
        self.loop_end(&end);
        let dict = self.temp();
        self.line(&format!("{dict} = load ptr, ptr {slot}"));
        self.assign(dest, &dict)
    }

    /// `dict[key] = value` on the dict at `place`. Changing a key's value in a dict nothing else
    /// holds happens here, and everything else in the runtime.
    pub(crate) fn dict_store(
        &mut self,
        place: &str,
        (key_word, value_word): (Word, Word),
        key: &Operand,
        value: Option<&Operand>,
    ) -> Result<(), String> {
        self.dict_store_at(place, (key_word, value_word), key, value, None)
    }

    /// As [`Self::dict_store`], with where the key's entry is when a lookup just found it.
    pub(crate) fn dict_store_at(
        &mut self,
        place: &str,
        (key_word, value_word): (Word, Word),
        key: &Operand,
        value: Option<&Operand>,
        found: Option<String>,
    ) -> Result<(), String> {
        let key = self.key_value(key, key_word)?;
        let held = match value {
            Some(value) => self.operand(value)?.0,
            None => String::new(),
        };
        self.dict_store_word(place, (key_word, value_word), &key, &held, found)
    }

    /// As [`Self::dict_store_at`], with the key and value already worked out.
    fn dict_store_word(
        &mut self,
        place: &str,
        (key_word, value_word): (Word, Word),
        key: &str,
        held: &str,
        found: Option<String>,
    ) -> Result<(), String> {
        let (key, held) = (key.to_owned(), held.to_owned());
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        self.declare("declare ptr @fennec_dict_set(ptr, i64, i64) nounwind");
        let dict = self.temp();
        let (quick, change, slow, done) =
            (self.label("quick"), self.label("change"), self.label("slow"), self.label("done"));
        self.line(&format!("{dict} = load ptr, ptr {place}"));
        let alone = self.alone(&dict);
        self.line(&format!("br i1 {alone}, label %{quick}, label %{slow}"));
        self.start_block(&quick);
        // With the dict alone, it's the same block the lookup searched, unchanged since.
        let entry = match found {
            Some(entry) => entry,
            None => self.find(&dict, key_word, &key),
        };
        let missing = self.temp();
        self.line(&format!("{missing} = icmp eq ptr {entry}, null"));
        // A short new key held inline, or a plain key compared by its words, for values holding
        // no block, goes straight in.
        let words_key = matches!(key_word, Word::Plain(id) if !self.key_by_value(id));
        let absent = if (key_word == Word::Text || words_key) && plain_value(value_word) {
            self.label("absent")
        } else {
            slow.clone()
        };
        self.line(&format!("br i1 {missing}, label %{absent}, label %{change}"));
        if absent != slow && words_key {
            self.start_block(&absent);
            // The dict keeps the key as a block of its own, made once here: given to the runtime
            // with it, when there's no room for it here.
            let add = self.add_plain_key_helper();
            let hash = self.hash(key_word, &key);
            let key_held = self.key_word(key_word, &key)?;
            let word = self.dict_word(&value_kind, &held, false);
            let (added, not_added) = (self.temp(), self.label("not_added"));
            self.line(&format!(
                "{added} = call i1 @{add}(ptr {dict}, i64 {hash}, i64 {key_held}, i64 {word})"
            ));
            self.line(&format!("br i1 {added}, label %{done}, label %{not_added}"));
            self.start_block(&not_added);
            let changed = self.temp();
            self.line(&format!(
                "{changed} = call ptr @fennec_dict_set(ptr {dict}, i64 {key_held}, i64 {word})"
            ));
            self.line(&format!("store ptr {changed}, ptr {place}"));
            self.line(&format!("br label %{done}"));
        } else if absent != slow {
            self.start_block(&absent);
            let add = self.add_short_key_helper(KeyBytes::Header);
            let (bytes, length) = (self.header_bytes(&key), self.header_byte_length(&key));
            let word = self.dict_word(&value_kind, &held, false);
            let added = self.temp();
            self.line(&format!(
                "{added} = call i1 @{add}(ptr {dict}, ptr {bytes}, i64 {length}, i64 {word})"
            ));
            self.line(&format!("br i1 {added}, label %{done}, label %{slow}"));
        }
        self.start_block(&change);
        let value_at = self.temp();
        self.line(&format!(
            "{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {}",
            entry_value(key_word)
        ));
        if value_kind.in_memory() {
            let word = self.dict_word(&value_kind, &held, true);
            let (old, block) = (self.temp(), self.temp());
            self.line(&format!("{old} = load ptr, ptr {value_at}"));
            self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
            self.line(&format!("store ptr {block}, ptr {value_at}"));
            self.release(&old);
        } else if value_kind.counted() {
            let held = if value_kind == Kind::TextBlock {
                self.owned_text_block(&held)
            } else {
                self.retain_value(&held, &value_kind);
                held.clone()
            };
            let old = self.temp();
            self.line(&format!("{old} = load ptr, ptr {value_at}"));
            self.line(&format!("store ptr {held}, ptr {value_at}"));
            self.release(&old);
        } else if value_kind != Kind::Unit {
            self.store_field(&value_kind, &held, &value_at, "");
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        let value_held = self.dict_word(&value_kind, &held, true);
        let changed = self.temp();
        if key_word == Word::Text {
            // The key's header goes in as its words, with an owner of its block if it has one.
            self.retain_text(&key);
            let [a, b, c] = [0, 1, 2].map(|at| self.header_word(&key, at));
            self.declare("declare ptr @fennec_dict_set_header(ptr, i64, i64, i64, i64) nounwind");
            self.line(&format!(
                "{changed} = call ptr @fennec_dict_set_header(ptr {dict}, i64 {a}, i64 {b}, i64 {c}, i64 {value_held})"
            ));
        } else {
            let key_held = self.key_word(key_word, &key)?;
            self.line(&format!(
                "{changed} = call ptr @fennec_dict_set(ptr {dict}, i64 {key_held}, i64 {value_held})"
            ));
        }
        self.line(&format!("store ptr {changed}, ptr {place}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// `dict[key]`, failing if it's not there.
    pub(crate) fn dict_read(
        &mut self,
        dest: LocalId,
        dict: &str,
        (key_word, value_word): (Word, Word),
        key: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let key = self.key_value(key, key_word)?;
        let entry = self.find(dict, key_word, &key);
        let missing = self.temp();
        let (absent, found) = (self.label("absent"), self.label("found"));
        self.line(&format!("{missing} = icmp eq ptr {entry}, null"));
        self.line(&format!("br i1 {missing}, label %{absent}, label %{found}, !prof !0"));
        self.start_block(&absent);
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        if key_word == Word::Text {
            self.declare("declare void @fennec_dict_missing_header(ptr, i64, i64, i64, i8, i64, i64) #1");
            let [a, b, c] = [0, 1, 2].map(|at| self.header_word(&key, at));
            self.line(&format!(
                "call void @fennec_dict_missing_header(ptr {dict}, i64 {a}, i64 {b}, i64 {c}, i8 0, i64 {from}, i64 {to})"
            ));
        } else {
            self.declare("declare void @fennec_dict_missing(ptr, i64, i8, i64, i64) #1");
            let shown = self.key_word(key_word, &key)?;
            self.line(&format!(
                "call void @fennec_dict_missing(ptr {dict}, i64 {shown}, i8 0, i64 {from}, i64 {to})"
            ));
        }
        let failed = self.failed_exit();
        self.line(&failed);
        self.start_block(&found);
        let value_at = self.temp();
        self.line(&format!(
            "{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {}",
            entry_value(key_word)
        ));
        let value_at = self.value_place(&value_kind, &value_at);
        self.read_into(dest, &value_kind, &value_at, "")
    }

    /// Where a value is, from where a dict keeps it: a plain struct or enum is in a block.
    fn value_place(&mut self, kind: &Kind, at: &str) -> String {
        if !kind.in_memory() {
            return at.to_owned();
        }
        let block = self.temp();
        self.line(&format!("{block} = load ptr, ptr {at}"));
        self.offset(&block, crate::lists::FIELDS_OFFSET)
    }

    /// Notes where a lookup found a key's entry, or null, for a store to the same key right
    /// after.
    fn remember_entry(&mut self, (dict, key): &(Operand, Operand), entry: &str) {
        if let Operand::Local(dict) = dict {
            self.found_entries.push((*dict, key.clone(), entry.to_owned()));
        }
    }

    /// `dict.get(key)`: the value as an optional, `None` when the key isn't there.
    fn dict_get_optional(&mut self, dest: LocalId, dict: &Operand, key: &Operand) -> Result<(), String> {
        let looked_up = (dict.clone(), key.clone());
        let (dict, kind) = self.operand(dict)?;
        let Kind::Dict(key_word, value_word) = kind else {
            return Err("`dict.get` on something else".to_owned());
        };
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let optional = self.kind_of(dest)?.clone();
        if matches!(optional, Kind::Maybe(_) | Kind::MaybeText) {
            let key = self.key_value(key, key_word)?;
            let entry = self.find(&dict, key_word, &key);
            self.remember_entry(&looked_up, &entry);
            let missing = self.temp();
            let (absent, found, done) = (self.label("absent"), self.label("found"), self.label("done"));
            self.line(&format!("{missing} = icmp eq ptr {entry}, null"));
            self.line(&format!("br i1 {missing}, label %{absent}, label %{found}"));
            self.start_block(&absent);
            self.copy(dest, &Operand::None)?;
            self.line(&format!("br label %{done}"));
            self.start_block(&found);
            let value_at = self.temp();
            self.line(&format!(
                "{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {}",
                entry_value(key_word)
            ));
            let value_at = self.value_place(&value_kind, &value_at);
            let value_at = self.value_place(&value_kind, &value_at);
            self.read_into(dest, &value_kind, &value_at, "")?;
            self.line(&format!("br label %{done}"));
            self.start_block(&done);
            return Ok(());
        }
        if !matches!(optional, Kind::Enum(_)) {
            return Err(format!("`dict.get` gives a {optional:?}, which this backend doesn't take yet"));
        }
        let (number, fields) = self.case_of(&optional, "Some")?;
        let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
        let key = self.key_value(key, key_word)?;
        let entry = self.find(&dict, key_word, &key);
        self.remember_entry(&looked_up, &entry);
        let missing = self.temp();
        let (absent, found, done) = (self.label("absent"), self.label("found"), self.label("done"));
        self.line(&format!("{missing} = icmp eq ptr {entry}, null"));
        self.line(&format!("br i1 {missing}, label %{absent}, label %{found}"));
        self.start_block(&absent);
        self.copy(dest, &Operand::None)?;
        self.line(&format!("br label %{done}"));
        self.start_block(&found);
        self.release_held(dest)?;
        let (slot, memory) = (self.slot(dest), optional.memory(self.module.layouts));
        self.line(&format!("store {memory} zeroinitializer, ptr {slot}"));
        self.line(&format!("store i64 {number}, ptr {slot}"));
        let value_at = self.temp();
        self.line(&format!(
            "{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {}",
            entry_value(key_word)
        ));
        let value = self.dict_part(&value_kind, &value_at);
        let (value_kind, value, owned) = self.as_header(&value_kind, &value);
        if value_kind.counted() && !owned {
            self.retain_value(&value, &value_kind);
        }
        let at = self.offset(&slot, offset);
        self.store_field(&value_kind, &value, &at, "");
        if value_kind.in_memory() {
            self.retain_inline(&value_kind, &at);
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// Where a key that's a slice of a string is: the string's address, and the slice's start and
    /// end in characters.
    fn slice_place(&mut self, (text, start, end): &crate::analysis::SliceKey) -> Result<[String; 3], String> {
        let (text, _) = self.operand(text)?;
        let mut at = |part: &Option<Operand>| -> Result<String, String> {
            Ok(match part {
                Some(part) => self.operand(part)?.0,
                None => i64::MIN.to_string(),
            })
        };
        Ok([text, at(start)?, at(end)?])
    }

    /// Where the value of the key that's the slice `text[from:to]` is, or null. In ASCII text a
    /// character is a byte, so the slice's bytes are hashed as a stored key's are and looked up
    /// right here; other text asks the runtime, as does an inline header.
    /// Defines `@fen_add_short_<from>(dict, bytes, length, value)`, which adds a key of 23 bytes
    /// or fewer to a dict that nothing else holds and that hasn't got it, when the dict has room:
    /// as an inline header, with a value that holds no block. Gives back whether it did, the
    /// runtime doing anything else, as its `set_inline` does the same. Its words and hash come
    /// from [`short_key`], as a lookup's do. `from` says where the bytes are.
    fn add_short_key_helper(&mut self, from: KeyBytes) -> String {
        self.string_slice_helpers();
        let header = header_bytes();
        let name = format!("fen_add_short_{}", from.name());
        let key = short_key(from);
        self.module.add_helper(
            &name,
            format!(
                "define internal i1 @{name}(ptr %d, ptr %bytes, i64 %length, i64 %value) #0 {{
entry:
  %short = icmp ule i64 %length, 23
  br i1 %short, label %hashed, label %no
hashed:
  %how_at = getelementptr inbounds i8, ptr %d, i64 {HOW_HASHED}
  %how = load i64, ptr %how_at
  %is_fast = icmp eq i64 %how, 0
  br i1 %is_fast, label %room, label %no
room:
  %used_at = getelementptr inbounds i8, ptr %d, i64 {USED}
  %used = load i64, ptr %used_at
  %mask_at = getelementptr inbounds i8, ptr %d, i64 {MASK}
  %mask = load i64, ptr %mask_at
  %count = add i64 %mask, 1
  %quarter = lshr i64 %count, 2
  %holds = sub i64 %count, %quarter
  %fits = icmp ult i64 %used, %holds
  br i1 %fits, label %add, label %no
add:
{key}  %slots = getelementptr inbounds i8, ptr %d, i64 {header}
  %first = and i64 %hash, %mask
  br label %probe
probe:
  %at = phi i64 [ %first, %add ], [ %after, %taken ]
  %slot = getelementptr inbounds i64, ptr %slots, i64 %at
  %held = load i64, ptr %slot
  %empty = icmp eq i64 %held, 0
  br i1 %empty, label %place, label %taken
taken:
  %stepped = add i64 %at, 1
  %after = and i64 %stepped, %mask
  %gone = sub i64 %after, %first
  %distance = and i64 %gone, %mask
  %too_far = icmp ugt i64 %distance, {LONGEST_PROBE}
  br i1 %too_far, label %no, label %probe
place:
  %number = add i64 %used, 1
  store i64 %number, ptr %slot
  %entries = getelementptr inbounds i64, ptr %slots, i64 %count
  %added = getelementptr inbounds [5 x i64], ptr %entries, i64 %used
  store i64 %hash, ptr %added
  %a_to = getelementptr inbounds i64, ptr %added, i64 1
  store i64 %a, ptr %a_to
  %b_to = getelementptr inbounds i64, ptr %added, i64 2
  store i64 %b, ptr %b_to
  %c_to = getelementptr inbounds i64, ptr %added, i64 3
  store i64 %c, ptr %c_to
  %value_to = getelementptr inbounds i64, ptr %added, i64 4
  store i64 %value, ptr %value_to
  %keys_at = getelementptr inbounds i8, ptr %d, i64 {KEYS}
  %keys = load i64, ptr %keys_at
  %more_keys = add i64 %keys, 1
  store i64 %more_keys, ptr %keys_at
  store i64 %number, ptr %used_at
  ret i1 true
no:
  ret i1 false
}}

"
            ),
        );
        name
    }

    /// Defines `@fen_add_plain(dict, hash, key, value)`, which adds a key that isn't there, the
    /// address of a plain struct's block compared by its words, to a dict that's alone, hashed
    /// the fast way and with room, and says whether it did. Anything else, it leaves alone for
    /// the runtime: a dict to grow, or a key too far from its slot, which the runtime answers by
    /// hashing the dict the strong way.
    fn add_plain_key_helper(&mut self) -> String {
        let header = header_bytes();
        let name = "fen_add_plain";
        self.module.add_helper(
            name,
            format!(
                "define internal i1 @{name}(ptr %d, i64 %hash, i64 %key, i64 %value) #0 {{
entry:
  %how_at = getelementptr inbounds i8, ptr %d, i64 {HOW_HASHED}
  %how = load i64, ptr %how_at
  %is_fast = icmp eq i64 %how, 0
  br i1 %is_fast, label %room, label %no
room:
  %used_at = getelementptr inbounds i8, ptr %d, i64 {USED}
  %used = load i64, ptr %used_at
  %mask_at = getelementptr inbounds i8, ptr %d, i64 {MASK}
  %mask = load i64, ptr %mask_at
  %count = add i64 %mask, 1
  %quarter = lshr i64 %count, 2
  %holds = sub i64 %count, %quarter
  %fits = icmp ult i64 %used, %holds
  br i1 %fits, label %add, label %no
add:
  %slots = getelementptr inbounds i8, ptr %d, i64 {header}
  %first = and i64 %hash, %mask
  br label %probe
probe:
  %at = phi i64 [ %first, %add ], [ %after, %taken ]
  %slot = getelementptr inbounds i64, ptr %slots, i64 %at
  %held = load i64, ptr %slot
  %empty = icmp eq i64 %held, 0
  br i1 %empty, label %place, label %taken
taken:
  %stepped = add i64 %at, 1
  %after = and i64 %stepped, %mask
  %gone = sub i64 %after, %first
  %distance = and i64 %gone, %mask
  %too_far = icmp ugt i64 %distance, {LONGEST_PROBE}
  br i1 %too_far, label %no, label %probe
place:
  %number = add i64 %used, 1
  store i64 %number, ptr %slot
  %entries = getelementptr inbounds i64, ptr %slots, i64 %count
  %added = getelementptr inbounds [{ENTRY_WORDS} x i64], ptr %entries, i64 %used
  store i64 %hash, ptr %added
  %key_to = getelementptr inbounds i64, ptr %added, i64 1
  store i64 %key, ptr %key_to
  %value_to = getelementptr inbounds i64, ptr %added, i64 2
  store i64 %value, ptr %value_to
  %keys_at = getelementptr inbounds i8, ptr %d, i64 {KEYS}
  %keys = load i64, ptr %keys_at
  %more_keys = add i64 %keys, 1
  store i64 %more_keys, ptr %keys_at
  store i64 %number, ptr %used_at
  ret i1 true
no:
  ret i1 false
}}

"
            ),
        );
        name.to_owned()
    }

    /// Defines `@fen_find_short_<from>(dict, bytes, length)`: the entry of a key of 23 bytes or
    /// fewer in a dict whose keys are held as headers, or null. The key's header words are made
    /// once, with no copy, and each entry the probe meets is compared with them a word at a time,
    /// as two inline headers are equal exactly when their words are.
    fn find_short_key_helper(&mut self, from: KeyBytes) -> String {
        self.string_slice_helpers();
        let header = header_bytes();
        let name = format!("fen_find_short_{}", from.name());
        let key = short_key(from);
        self.module.add_helper(
            &name,
            format!(
                "define internal ptr @{name}(ptr %d, ptr %bytes, i64 %length) #0 {{
entry:
{key}  %how_at = getelementptr inbounds i8, ptr %d, i64 {HOW_HASHED}
  %how = load i64, ptr %how_at
  %is_strong = icmp ne i64 %how, 0
  br i1 %is_strong, label %rehash, label %start, !prof !0
rehash:
{STRONG_BYTES_OF_BYTES}  br label %start
start:
  %probe_hash = phi i64 [ %hash, %entry ], [ %strong_hash, %rehash ]
  %mask_at = getelementptr inbounds i8, ptr %d, i64 {MASK}
  %mask = load i64, ptr %mask_at
  %slots = getelementptr inbounds i8, ptr %d, i64 {header}
  %count = add i64 %mask, 1
  %first = and i64 %probe_hash, %mask
  %entries = getelementptr inbounds i64, ptr %slots, i64 %count
  br label %head
head:
  %at = phi i64 [ %first, %start ], [ %after, %next ]
  %slot = getelementptr inbounds i64, ptr %slots, i64 %at
  %taken = load i64, ptr %slot
  %empty = icmp eq i64 %taken, 0
  br i1 %empty, label %none, label %check
none:
  ret ptr null
check:
  %number = add i64 %taken, -1
  %entry_at = getelementptr inbounds [5 x i64], ptr %entries, i64 %number
  %theirs = load i64, ptr %entry_at
  %same_hash = icmp eq i64 %theirs, %probe_hash
  br i1 %same_hash, label %compare, label %next
compare:
  %k0_at = getelementptr inbounds i64, ptr %entry_at, i64 1
  %k0 = load i64, ptr %k0_at
  %k1_at = getelementptr inbounds i64, ptr %entry_at, i64 2
  %k1 = load i64, ptr %k1_at
  %k2_at = getelementptr inbounds i64, ptr %entry_at, i64 3
  %k2 = load i64, ptr %k2_at
  %e0 = icmp eq i64 %k0, %a
  %e1 = icmp eq i64 %k1, %b
  %e2 = icmp eq i64 %k2, %c
  %e01 = and i1 %e0, %e1
  %same = and i1 %e01, %e2
  br i1 %same, label %found, label %next
next:
  %stepped = add i64 %at, 1
  %after = and i64 %stepped, %mask
  br label %head
found:
  ret ptr %entry_at
}}

"
            ),
        );
        name
    }

    /// Where an ASCII string's slice is and how many bytes it has, as slicing clamps it, and
    /// whether it is one: `false` for text that isn't ASCII or a header holding its text.
    fn ascii_slice_bytes(&mut self, [text, from, to]: &[String; 3]) -> (String, String, String) {
        let (block_path, joined) = (self.label("slice_bytes"), self.label("slice_bytes_done"));
        let came_from = self.current.clone();
        let text = if self.headers() {
            let in_block = self.header_in_block(text);
            self.line(&format!("br i1 {in_block}, label %{block_path}, label %{joined}"));
            self.start_block(&block_path);
            let (first, block) = (self.header_word(text, 0), self.temp());
            self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
            block
        } else {
            self.line(&format!("br label %{block_path}"));
            self.start_block(&block_path);
            text.clone()
        };
        let (bytes_at, chars_at) =
            (self.offset(&text, TEXT_BYTES.unsigned_abs()), self.offset(&text, TEXT_CHARS.unsigned_abs()));
        let (length, chars, ascii) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{length} = load i64, ptr {bytes_at}"));
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{ascii} = icmp eq i64 {length}, {chars}"));
        let start = self.clamp_place(from, &length, "0");
        let end = self.clamp_place(to, &length, &length);
        let (later, size, first) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{later} = call i64 @llvm.smax.i64(i64 {end}, i64 {start})"));
        self.line(&format!("{size} = sub i64 {later}, {start}"));
        let text_start = self.offset(&text, TEXT.unsigned_abs());
        self.line(&format!("{first} = getelementptr inbounds i8, ptr {text_start}, i64 {start}"));
        let block_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        if !self.headers() {
            return (ascii, first, size);
        }
        let (ok, bytes, count) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{ok} = phi i1 [ false, %{came_from} ], [ {ascii}, %{block_end} ]"));
        self.line(&format!("{bytes} = phi ptr [ null, %{came_from} ], [ {first}, %{block_end} ]"));
        self.line(&format!("{count} = phi i64 [ 0, %{came_from} ], [ {size}, %{block_end} ]"));
        (ok, bytes, count)
    }

    fn slice_value_at(&mut self, dict: &str, key_word: Word, [text, from, to]: &[String; 3]) -> String {
        self.string_slice_helpers();
        let (here, there, joined) = (self.label("ascii"), self.label("other"), self.label("looked"));
        let header = self.headers().then(|| text.clone());
        let text = &match &header {
            Some(header) => {
                let in_block = self.header_in_block(header);
                let block_path = self.label("slice_block");
                self.line(&format!("br i1 {in_block}, label %{block_path}, label %{there}, !prof !8"));
                self.start_block(&block_path);
                let first = self.header_word(header, 0);
                let block = self.temp();
                self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
                block
            }
            None => text.clone(),
        };
        let (bytes_at, chars_at) =
            (self.offset(text, TEXT_BYTES.unsigned_abs()), self.offset(text, TEXT_CHARS.unsigned_abs()));
        let (length, chars, ascii) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{length} = load i64, ptr {bytes_at}"));
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{ascii} = icmp eq i64 {length}, {chars}"));
        self.line(&format!("br i1 {ascii}, label %{here}, label %{there}, !prof !8"));
        self.start_block(&here);
        // Slicing's clamping: a missing end, then from the end if negative, then within.
        let start = self.clamp_place(from, &length, "0");
        let end = self.clamp_place(to, &length, &length);
        let (later, size) = (self.temp(), self.temp());
        self.line(&format!("{later} = call i64 @llvm.smax.i64(i64 {end}, i64 {start})"));
        self.line(&format!("{size} = sub i64 {later}, {start}"));
        let text_start = self.offset(text, TEXT.unsigned_abs());
        let first = self.temp();
        self.line(&format!("{first} = getelementptr inbounds i8, ptr {text_start}, i64 {start}"));
        let entry = if key_word == Word::Text {
            self.find_text_key(dict, (&first, KeyBytes::InBlock), &size)
        } else {
            let (mixed, high, folded, hash, entry) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{mixed} = call i64 @fen_slice_hash(ptr {first}, i64 {size})"));
            self.line(&format!("{high} = lshr i64 {mixed}, 29"));
            self.line(&format!("{folded} = xor i64 {mixed}, {high}"));
            self.line(&format!("{hash} = lshr i64 {folded}, 1"));
            self.line(&format!(
                "{entry} = call ptr @fen_find_slice(ptr {dict}, ptr {first}, i64 {size}, i64 {hash})"
            ));
            entry
        };
        let (none, value_place, found) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{none} = icmp eq ptr {entry}, null"));
        self.line(&format!(
            "{value_place} = getelementptr inbounds i8, ptr {entry}, i64 {}",
            entry_value(key_word)
        ));
        self.line(&format!("{found} = select i1 {none}, ptr null, ptr {value_place}"));
        let here_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&there);
        self.declare("declare ptr @fennec_dict_find_slice(ptr, ptr, i64, i64) nounwind");
        let asked = self.on_blocks([header.as_deref().unwrap_or(text)], |emit, [text]| {
            let asked = emit.temp();
            emit.line(&format!(
                "{asked} = call ptr @fennec_dict_find_slice(ptr {dict}, ptr {text}, i64 {from}, i64 {to})"
            ));
            asked
        });
        let there_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let value_at = self.temp();
        self.line(&format!("{value_at} = phi ptr [ {found}, %{here_end} ], [ {asked}, %{there_end} ]"));
        value_at
    }

    /// A slice's end as slicing clamps it into `0..=length`, `missing` standing for `i64::MIN`.
    fn clamp_place(&mut self, at: &str, length: &str, missing: &str) -> String {
        let (absent, negative, from_end, low, high, clamped, chosen) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{absent} = icmp eq i64 {at}, {}", i64::MIN));
        self.line(&format!("{negative} = icmp slt i64 {at}, 0"));
        self.line(&format!("{from_end} = add i64 {at}, {length}"));
        self.line(&format!("{low} = call i64 @llvm.smax.i64(i64 {from_end}, i64 0)"));
        self.line(&format!("{high} = call i64 @llvm.smin.i64(i64 {at}, i64 {length})"));
        self.line(&format!("{clamped} = select i1 {negative}, i64 {low}, i64 {high}"));
        self.line(&format!("{chosen} = select i1 {absent}, i64 {missing}, i64 {clamped}"));
        chosen
    }

    /// Hashing bytes where they are, as a stored key's block is hashed, and finding them.
    fn string_slice_helpers(&mut self) {
        self.strong_hash_helpers();
        self.declare("@fennec_hash_seed = external dso_local global i64");
        self.declare("declare i64 @llvm.fshl.i64(i64, i64, i64)");
        self.declare("declare i64 @llvm.smax.i64(i64, i64)");
        self.declare("declare i64 @llvm.smin.i64(i64, i64)");
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        self.declare("declare i32 @memcmp(ptr, ptr, i64)");
        // Whole words straight from the bytes, the last part word through a zeroed one, so
        // nothing past the slice is read.
        self.module.add_helper(
            "fen_slice_hash",
            format!(
                "define internal i64 @fen_slice_hash(ptr %bytes, i64 %length) #0 {{\nentry:\n  %part = alloca i64\n{SEEDED_LENGTH}  %start = mul i64 %seeded, {MIX}\n  %whole = lshr i64 %length, 3\n  br label %head\nhead:\n  %hash = phi i64 [ %start, %entry ], [ %mixed, %body ]\n  %at = phi i64 [ 0, %entry ], [ %next, %body ]\n  %more = icmp ult i64 %at, %whole\n  br i1 %more, label %body, label %tail\nbody:\n  %place = getelementptr inbounds i64, ptr %bytes, i64 %at\n  %word = load i64, ptr %place, align 1\n  %turned = call i64 @llvm.fshl.i64(i64 %hash, i64 %hash, i64 23)\n  %xored = xor i64 %turned, %word\n  %mixed = mul i64 %xored, {MIX}\n  %next = add i64 %at, 1\n  br label %head\ntail:\n  %rest = and i64 %length, 7\n  %some = icmp ne i64 %rest, 0\n  br i1 %some, label %last, label %done\nlast:\n  store i64 0, ptr %part\n  %from = getelementptr inbounds i64, ptr %bytes, i64 %whole\n  call void @llvm.memcpy.p0.p0.i64(ptr %part, ptr %from, i64 %rest, i1 false)\n  %last_word = load i64, ptr %part\n  %last_turned = call i64 @llvm.fshl.i64(i64 %hash, i64 %hash, i64 23)\n  %last_xored = xor i64 %last_turned, %last_word\n  %last_mixed = mul i64 %last_xored, {MIX}\n  ret i64 %last_mixed\ndone:\n  ret i64 %hash\n}}\n\n"
            ),
        );
        let mut find = find_start("fen_find_slice", "ptr", STRONG_BYTES)
            .replace("(ptr %d, ptr %key, i64 %given)", "(ptr %d, ptr %key, i64 %length, i64 %given)");
        find += &format!(
            "  %other = load ptr, ptr %other_at\n  %their_length_at = getelementptr inbounds i8, ptr %other, i64 {TEXT_BYTES}\n  %their_length = load i64, ptr %their_length_at\n  %same_length = icmp eq i64 %their_length, %length\n  br i1 %same_length, label %bytes, label %next\nbytes:\n  %their_text = getelementptr inbounds i8, ptr %other, i64 {TEXT}\n  %order = call i32 @memcmp(ptr %their_text, ptr %key, i64 %length)\n  %equal = icmp eq i32 %order, 0\n  br i1 %equal, label %found, label %next\n}}\n\n"
        );
        self.module.add_helper("fen_find_slice", find);
    }

    /// `dict.get(text[a:b])`, finding those bytes where they are.
    fn slice_get(
        &mut self,
        dest: LocalId,
        (dict, key_local): (&Operand, &Operand),
        key: &crate::analysis::SliceKey,
    ) -> Result<(), String> {
        let looked_up = (dict.clone(), key_local.clone());
        let (dict, kind) = self.operand(dict)?;
        let Kind::Dict(key_word, value_word) = kind else {
            return Err("`dict.get` on something else".to_owned());
        };
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let place = self.slice_place(key)?;
        let value_at = self.slice_value_at(&dict, key_word, &place);
        // A store with the same slice next finds the value here, as nothing's changed between.
        self.remember_entry(&looked_up, &value_at);
        let missing = self.temp();
        self.line(&format!("{missing} = icmp eq ptr {value_at}, null"));
        let (absent, found, done) = (self.label("absent"), self.label("found"), self.label("done"));
        self.line(&format!("br i1 {missing}, label %{absent}, label %{found}"));
        self.start_block(&absent);
        self.copy(dest, &Operand::None)?;
        self.line(&format!("br label %{done}"));
        self.start_block(&found);
        let optional = self.kind_of(dest)?.clone();
        if matches!(optional, Kind::Maybe(_) | Kind::MaybeText) {
            let value_at = self.value_place(&value_kind, &value_at);
            let value_at = self.value_place(&value_kind, &value_at);
            self.read_into(dest, &value_kind, &value_at, "")?;
        } else {
            let (number, fields) = self.case_of(&optional, "Some")?;
            let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
            self.release_held(dest)?;
            let (slot, memory) = (self.slot(dest), optional.memory(self.module.layouts));
            self.line(&format!("store {memory} zeroinitializer, ptr {slot}"));
            self.line(&format!("store i64 {number}, ptr {slot}"));
            let value = self.load_field(&value_kind, &value_at, "");
            let (value_kind, value, owned) = self.as_header(&value_kind, &value);
            if value_kind.counted() && !owned {
                self.retain_value(&value, &value_kind);
            }
            let at = self.offset(&slot, offset);
            self.store_field(&value_kind, &value, &at, "");
            if value_kind.in_memory() {
                self.retain_inline(&value_kind, &at);
            }
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// `dict[text[a:b]] = value`, a key block made only when the key is new.
    pub(crate) fn slice_set(
        &mut self,
        place: &str,
        (key_word, value_word): (Word, Word),
        (key, found): (&crate::analysis::SliceKey, Option<String>),
        value: &Operand,
    ) -> Result<(), String> {
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let (held, fresh) = self.value_for(value, &value_kind)?;
        let sliced = self.slice_place(key)?;
        let [text, from, to] = sliced.clone();
        let dict = self.temp();
        self.line(&format!("{dict} = load ptr, ptr {place}"));
        // A key that's there, in a dict nothing else holds, just gets its new value.
        let value_at = match found {
            Some(value_at) => value_at,
            None => self.slice_value_at(&dict, key_word, &sliced),
        };
        let alone = self.alone(&dict);
        let (there, quick) = (self.temp(), self.temp());
        self.line(&format!("{there} = icmp ne ptr {value_at}, null"));
        self.line(&format!("{quick} = and i1 {alone}, {there}"));
        let (change, add, done) = (self.label("change"), self.label("add"), self.label("stored"));
        self.line(&format!("br i1 {quick}, label %{change}, label %{add}"));
        self.start_block(&change);
        if value_kind.in_memory() {
            let word = self.dict_word(&value_kind, &held, true);
            let (old, block) = (self.temp(), self.temp());
            self.line(&format!("{old} = load ptr, ptr {value_at}"));
            self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
            self.line(&format!("store ptr {block}, ptr {value_at}"));
            self.release(&old);
        } else if value_kind.counted() {
            if !fresh {
                self.retain_value(&held, &value_kind);
            }
            let old = self.temp();
            self.line(&format!("{old} = load ptr, ptr {value_at}"));
            self.line(&format!("store ptr {held}, ptr {value_at}"));
            self.release_kind(&old, &value_kind);
        } else if value_kind != Kind::Unit {
            self.store_field(&value_kind, &held, &value_at, "");
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&add);
        // A short new key of an ASCII string, for values holding no block, goes straight in.
        if key_word == Word::Text && plain_value(value_word) {
            let (ok, bytes, size) = self.ascii_slice_bytes(&sliced);
            let word = self.dict_word(&value_kind, &held, false);
            let (missing, try_it) = (self.temp(), self.temp());
            self.line(&format!("{missing} = xor i1 {there}, true"));
            let both = self.temp();
            self.line(&format!("{both} = and i1 {alone}, {missing}"));
            self.line(&format!("{try_it} = and i1 {both}, {ok}"));
            let (native, runtime) = (self.label("add_here"), self.label("add_runtime"));
            self.line(&format!("br i1 {try_it}, label %{native}, label %{runtime}"));
            self.start_block(&native);
            let add = self.add_short_key_helper(KeyBytes::InBlock);
            let added = self.temp();
            self.line(&format!("{added} = call i1 @{add}(ptr {dict}, ptr {bytes}, i64 {size}, i64 {word})"));
            self.line(&format!("br i1 {added}, label %{done}, label %{runtime}"));
            self.start_block(&runtime);
        }
        // Mostly a new key, so the runtime doesn't look for it again before adding it; it still
        // finds one that's there, in a dict something else holds.
        let word = self.dict_word(&value_kind, &held, !fresh);
        self.declare("declare ptr @fennec_dict_add_new_slice(ptr, ptr, i64, i64, i64) nounwind");
        let changed = self.on_blocks([text.as_str()], |emit, [text]| {
            let changed = emit.temp();
            emit.line(&format!(
                "{changed} = call ptr @fennec_dict_add_new_slice(ptr {dict}, ptr {text}, i64 {from}, i64 {to}, i64 {word})"
            ));
            changed
        });
        self.line(&format!("store ptr {changed}, ptr {place}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// `dict.pop(key)`: the runtime takes the key out, failing as the interpreter does if it
    /// isn't there, and hands back its value with its owner.
    /// `dict.clear()` or `set.clear()`: the runtime takes over the owner and gives back an empty
    /// one's.
    pub(crate) fn dict_clear(&mut self, dict: LocalId) {
        self.declare("declare ptr @fennec_set_clear(ptr) nounwind");
        let place = self.slot(dict);
        let (held, empty) = (self.temp(), self.temp());
        self.line(&format!("{held} = load ptr, ptr {place}"));
        self.line(&format!("{empty} = call ptr @fennec_set_clear(ptr {held})"));
        self.line(&format!("store ptr {empty}, ptr {place}"));
    }

    /// `set.discard(key)`, or `set.remove(key)` when `must`, which fails on a key that isn't there.
    pub(crate) fn set_remove(
        &mut self,
        set: LocalId,
        key: &Operand,
        must: bool,
        span: Span,
    ) -> Result<(), String> {
        let Kind::Dict(key_word, _) = self.kind_of(set)?.clone() else {
            return Err("a set's method on something else".to_owned());
        };
        let key = self.key_value(key, key_word)?;
        let place = self.slot(set);
        let (held, changed) = (self.temp(), self.temp());
        let (from, to, must) = (i64::from(span.start), i64::from(span.end), i8::from(must));
        self.line(&format!("{held} = load ptr, ptr {place}"));
        if key_word == Word::Text {
            // The key's owner stays here.
            self.declare("declare ptr @fennec_set_remove_header(ptr, i64, i64, i64, i8, i64, i64) nounwind");
            let [a, b, c] = [0, 1, 2].map(|at| self.header_word(&key, at));
            self.line(&format!(
                "{changed} = call ptr @fennec_set_remove_header(ptr {held}, i64 {a}, i64 {b}, i64 {c}, i8 {must}, i64 {from}, i64 {to})"
            ));
        } else {
            let key_held = self.key_word(key_word, &key)?;
            self.declare("declare ptr @fennec_set_remove(ptr, i64, i8, i64, i64) nounwind");
            self.line(&format!(
                "{changed} = call ptr @fennec_set_remove(ptr {held}, i64 {key_held}, i8 {must}, i64 {from}, i64 {to})"
            ));
        }
        self.leave_if_failed();
        self.line(&format!("store ptr {changed}, ptr {place}"));
        Ok(())
    }

    pub(crate) fn dict_pop(
        &mut self,
        dest: Option<LocalId>,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let [Operand::Local(dict), key] = args else { return Err("`dict.pop` on a literal".to_owned()) };
        let kind = self.kind_of(*dict)?.clone();
        let Kind::Dict(key_word, value_word) = kind else {
            return Err("`dict.pop` on something else".to_owned());
        };
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let key = self.key_value(key, key_word)?;
        self.declare("declare i64 @fennec_dict_popped() nounwind");
        let place = self.slot(*dict);
        let (held, changed, word) = (self.temp(), self.temp(), self.temp());
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!("{held} = load ptr, ptr {place}"));
        if key_word == Word::Text {
            // The key's owner stays here.
            self.declare("declare ptr @fennec_dict_pop_header(ptr, i64, i64, i64, i64, i64) nounwind");
            let [a, b, c] = [0, 1, 2].map(|at| self.header_word(&key, at));
            self.line(&format!(
                "{changed} = call ptr @fennec_dict_pop_header(ptr {held}, i64 {a}, i64 {b}, i64 {c}, i64 {from}, i64 {to})"
            ));
        } else {
            let key_held = self.key_word(key_word, &key)?;
            self.declare("declare ptr @fennec_dict_pop(ptr, i64, i64, i64) nounwind");
            self.line(&format!(
                "{changed} = call ptr @fennec_dict_pop(ptr {held}, i64 {key_held}, i64 {from}, i64 {to})"
            ));
        }
        self.line(&format!("store ptr {changed}, ptr {place}"));
        self.leave_if_failed();
        self.line(&format!("{word} = call i64 @fennec_dict_popped()"));
        if value_kind.in_memory() {
            let block = self.temp();
            self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
            if let Some(dest) = dest {
                let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                self.read_into(dest, &value_kind, &fields, "")?;
            }
            self.release(&block);
            return Ok(());
        }
        // The value's word, read back as what it holds, already with its owner.
        let at = self.place_for(&Kind::Int);
        self.line(&format!("store i64 {word}, ptr {at}"));
        let value = self.load_field(&value_kind, &at, "");
        // A string comes out as its block, with the owner the dict had: a header takes it over,
        // unless an optional's wanted, which holds the block.
        let wants_block = match dest {
            Some(dest) => matches!(self.kind_of(dest)?, Kind::Maybe(_)),
            None => false,
        };
        let (value_kind, value) = if value_kind == Kind::TextBlock && !wants_block {
            (Kind::Str, self.text_from_block(&value))
        } else {
            (value_kind, value)
        };
        match dest {
            Some(dest) if self.kind_of(dest)? == &value_kind => self.assign(dest, &value),
            // An optional string is the same header.
            Some(dest) if value_kind == Kind::Str && *self.kind_of(dest)? == Kind::MaybeText => {
                self.assign(dest, &value)
            }
            Some(dest) => {
                // Given where an optional's wanted.
                let optional = self.kind_of(dest)?.clone();
                if matches!(optional, Kind::Maybe(_)) {
                    return self.assign(dest, &value);
                }
                let (number, fields) = self.case_of(&optional, "Some")?;
                let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
                self.release_held(dest)?;
                let (slot, memory) = (self.slot(dest), optional.memory(self.module.layouts));
                self.line(&format!("store {memory} zeroinitializer, ptr {slot}"));
                self.line(&format!("store i64 {number}, ptr {slot}"));
                let into = self.offset(&slot, offset);
                self.store_field(&value_kind, &value, &into, "");
                Ok(())
            }
            None => {
                if value_kind.counted() {
                    self.release_kind(&value, &value_kind);
                }
                Ok(())
            }
        }
    }

    /// `dict.get(key, default)`, or `dict.get(key)` as an optional.
    pub(crate) fn dict_get(&mut self, dest: LocalId, args: &[Operand]) -> Result<(), String> {
        if let [dict, Operand::Local(key)] = args
            && let Some(sliced) = self.known.slice_keys.get(&key.0).cloned()
        {
            return self.slice_get(dest, (dict, &Operand::Local(*key)), &sliced);
        }
        if let [dict, key] = args {
            return self.dict_get_optional(dest, dict, key);
        }
        let [dict, key, default] = args else { return Err("`dict.get` with no default".to_owned()) };
        let looked_up = (dict.clone(), key.clone());
        let (dict, kind) = self.operand(dict)?;
        let Kind::Dict(key_word, value_word) = kind else {
            return Err("`dict.get` on something else".to_owned());
        };
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let key = self.key_value(key, key_word)?;
        let entry = self.find(&dict, key_word, &key);
        self.remember_entry(&looked_up, &entry);
        let missing = self.temp();
        let (absent, found, done) = (self.label("absent"), self.label("found"), self.label("done"));
        self.line(&format!("{missing} = icmp eq ptr {entry}, null"));
        self.line(&format!("br i1 {missing}, label %{absent}, label %{found}"));
        self.start_block(&absent);
        self.copy(dest, default)?;
        self.line(&format!("br label %{done}"));
        self.start_block(&found);
        let value_at = self.temp();
        self.line(&format!(
            "{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {}",
            entry_value(key_word)
        ));
        let value_at = self.value_place(&value_kind, &value_at);
        self.read_into(dest, &value_kind, &value_at, "")?;
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    pub(crate) fn dict_length(&mut self, dict: &str) -> String {
        let (at, length) = (self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {dict}, i64 {KEYS}"));
        self.line(&format!("{length} = load i64, ptr {at}"));
        length
    }

    /// `key in dict`.
    pub(crate) fn dict_contains(
        &mut self,
        op: BinOp,
        key: &Operand,
        dict: &Operand,
    ) -> Result<String, String> {
        let (dict, kind) = self.operand(dict)?;
        let Kind::Dict(key_word, _) = kind else { return Err("`in` on something else".to_owned()) };
        let key = self.key_value(key, key_word)?;
        let entry = self.find(&dict, key_word, &key);
        let result = self.temp();
        let cc = if op == BinOp::In { "ne" } else { "eq" };
        self.line(&format!("{result} = icmp {cc} ptr {entry}, null"));
        Ok(result)
    }

    /// `dict.items()`, `.keys()` or `.values()`: a new list, in the order keys were added.
    pub(crate) fn dict_list(&mut self, dest: LocalId, dict: &Operand, what: &str) -> Result<(), String> {
        let (dict, kind) = self.operand(dict)?;
        let Kind::Dict(key_word, value_word) = kind else {
            return Err("a dict's items of something else".to_owned());
        };
        let Kind::List(item) = self.kind_of(dest)?.clone() else {
            return Err("a dict's items, not as a list".to_owned());
        };
        let item_kind = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        let key_kind = self.word_kind(key_word).ok_or("a dict key this backend doesn't take yet")?;
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let length = self.dict_length(&dict);
        let list = self.new_list(item, &length);
        let memory = item_kind.memory(self.module.layouts);
        let (mask_at, mask, count, used_at, used, entries) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{mask_at} = getelementptr inbounds i8, ptr {dict}, i64 {MASK}"));
        self.line(&format!("{mask} = load i64, ptr {mask_at}"));
        self.line(&format!("{count} = add i64 {mask}, 1"));
        self.line(&format!("{used_at} = getelementptr inbounds i8, ptr {dict}, i64 {USED}"));
        self.line(&format!("{used} = load i64, ptr {used_at}"));
        let slots = self.temp();
        self.line(&format!("{slots} = getelementptr inbounds i8, ptr {dict}, i64 {}", header_bytes()));
        self.line(&format!("{entries} = getelementptr inbounds i64, ptr {slots}, i64 {count}"));
        let written = self.counter();
        let (at, end) = self.loop_begin(&used);
        let (entry, hash, removed) = (self.temp(), self.temp(), self.temp());
        let (live, skip) = (self.label("live"), self.label("skip"));
        self.line(&format!("{entry} = getelementptr inbounds i8, ptr {entries}, i64 0"));
        let entry_at = self.temp();
        self.line(&format!(
            "{entry_at} = getelementptr inbounds [{} x i64], ptr {entry}, i64 {at}",
            entry_words(key_word)
        ));
        self.line(&format!("{hash} = load i64, ptr {entry_at}"));
        self.line(&format!("{removed} = icmp eq i64 {hash}, {REMOVED}"));
        self.line(&format!("br i1 {removed}, label %{skip}, label %{live}"));
        self.start_block(&live);
        let place_number = self.temp();
        self.line(&format!("{place_number} = load i64, ptr {written}"));
        let first_item = self.items_of(&list);
        let place = self.temp();
        self.line(&format!(
            "{place} = getelementptr inbounds {memory}, ptr {first_item}, i64 {place_number}"
        ));
        let key_at = self.temp();
        self.line(&format!("{key_at} = getelementptr inbounds i8, ptr {entry_at}, i64 {ENTRY_KEY}"));
        let value_at = self.temp();
        self.line(&format!(
            "{value_at} = getelementptr inbounds i8, ptr {entry_at}, i64 {}",
            entry_value(key_word)
        ));
        // A set's entries hold no value, so only what's wanted is read.
        match what {
            "keys" => {
                let key_value = self.dict_part(&key_kind, &key_at);
                self.put_item(&item_kind, &key_kind, &key_value, &place);
            }
            "values" => {
                let value_value = self.dict_part(&value_kind, &value_at);
                self.put_item(&item_kind, &value_kind, &value_value, &place);
            }
            _ => {
                let key_value = self.dict_part(&key_kind, &key_at);
                let value_value = self.dict_part(&value_kind, &value_at);
                self.put_pair(&item_kind, &place, [(&key_kind, &key_value), (&value_kind, &value_value)])?;
            }
        }
        let next_place = self.temp();
        self.line(&format!("{next_place} = add i64 {place_number}, 1"));
        self.line(&format!("store i64 {next_place}, ptr {written}"));
        self.line(&format!("br label %{skip}"));
        self.start_block(&skip);
        self.loop_end(&end);
        self.assign(dest, &list)
    }

    /// A key or value from where a dict keeps it: a plain struct key is in a block of its own.
    fn dict_part(&mut self, kind: &Kind, at: &str) -> String {
        if kind.in_memory() {
            let (block, fields) = (self.temp(), self.temp());
            self.line(&format!("{block} = load ptr, ptr {at}"));
            self.line(&format!("{fields} = getelementptr inbounds i8, ptr {block}, i64 16"));
            return fields;
        }
        self.load_field(kind, at, "")
    }
}

fn word_ty(word: Word) -> Ty {
    match word {
        Word::Int => Ty::Int,
        Word::Float => Ty::Float,
        Word::U64 => Ty::U64,
        Word::F32 => Ty::F32,
        Word::Bool => Ty::Bool,
        Word::Str | Word::Text => Ty::Str,
        Word::Nothing => Ty::Unit,
        Word::Plain(id) => Ty::Struct(id),
        Word::Block(number) => Ty::Block(number),
    }
}

/// Whether a dict's values are words that hold no block, so adding one owns nothing.
fn plain_value(word: Word) -> bool {
    matches!(word, Word::Int | Word::Float | Word::Bool | Word::Nothing | Word::U64 | Word::F32)
}

/// Where a key's bytes are, which says how its words can be read.
#[derive(Clone, Copy)]
enum KeyBytes {
    /// A header copied whole into memory of its own: 24 bytes, zero after the text, with its tag
    /// in the last.
    Header,
    /// Inside a string's block, anywhere in its text: past the slice may be past the block, but
    /// the 8 bytes ending where the slice ends never are, as the block's header comes first.
    InBlock,
}

impl KeyBytes {
    fn name(self) -> &'static str {
        match self {
            KeyBytes::Header => "header",
            KeyBytes::InBlock => "in_block",
        }
    }
}

/// Lines working out a key of 23 bytes or fewer from `%bytes` and `%length`: `%a`, `%b` and `%c`,
/// the words of its inline header, tag and all; and `%hash`, as the runtime hashes its text, a
/// word at a time with the last zero after the text. Nothing is copied and nothing outside the
/// key's memory is read: a slice's last part word is the 8 bytes ending where it ends, moved down.
fn short_key(from: KeyBytes) -> String {
    let mut lines = String::new();
    match from {
        KeyBytes::Header => {
            lines += "  %a = load i64, ptr %bytes, align 1\n  %b_from = getelementptr inbounds i8, ptr %bytes, i64 8\n  %b = load i64, ptr %b_from, align 1\n  %c_from = getelementptr inbounds i8, ptr %bytes, i64 16\n  %c = load i64, ptr %c_from, align 1\n  %c_text = and i64 %c, 72057594037927935\n";
        }
        KeyBytes::InBlock => {
            lines += "  %tail_back = add i64 %length, -8\n  %tail_at = getelementptr i8, ptr %bytes, i64 %tail_back\n";
            for (at, word) in ["a", "b", "c_text"].iter().enumerate() {
                let start = at * 8;
                let end = start + 8;
                let _ = write!(
                    lines,
                    "  %{word}_whole = icmp uge i64 %length, {end}\n  %{word}_some = icmp ugt i64 %length, {start}\n  %{word}_at = getelementptr inbounds i8, ptr %bytes, i64 {start}\n  %{word}_from = select i1 %{word}_whole, ptr %{word}_at, ptr %tail_at\n  %{word}_raw = load i64, ptr %{word}_from, align 1\n  %{word}_kept = sub i64 %length, {start}\n  %{word}_dropped = sub i64 8, %{word}_kept\n  %{word}_bits = shl i64 %{word}_dropped, 3\n  %{word}_part_bits = select i1 %{word}_whole, i64 0, i64 %{word}_bits\n  %{word}_shift = select i1 %{word}_some, i64 %{word}_part_bits, i64 0\n  %{word}_moved = lshr i64 %{word}_raw, %{word}_shift\n  %{word} = select i1 %{word}_some, i64 %{word}_moved, i64 0\n"
                );
            }
            lines += "  %tag = or i64 %length, 128\n  %tag_high = shl i64 %tag, 56\n  %c = or i64 %c_text, %tag_high\n";
        }
    }
    let _ = write!(
        lines,
        "{SEEDED_LENGTH}  %h0 = mul i64 %seeded, {MIX}\n  %t0 = call i64 @llvm.fshl.i64(i64 %h0, i64 %h0, i64 23)\n  %x0 = xor i64 %t0, %a\n  %h1 = mul i64 %x0, {MIX}\n  %t1 = call i64 @llvm.fshl.i64(i64 %h1, i64 %h1, i64 23)\n  %x1 = xor i64 %t1, %b\n  %h2 = mul i64 %x1, {MIX}\n  %t2 = call i64 @llvm.fshl.i64(i64 %h2, i64 %h2, i64 23)\n  %x2 = xor i64 %t2, %c_text\n  %h3 = mul i64 %x2, {MIX}\n  %rounded = add i64 %length, 7\n  %words = lshr i64 %rounded, 3\n  %one = icmp eq i64 %words, 1\n  %two = icmp eq i64 %words, 2\n  %three = icmp eq i64 %words, 3\n  %pick3 = select i1 %three, i64 %h3, i64 %h0\n  %pick2 = select i1 %two, i64 %h2, i64 %pick3\n  %mixed = select i1 %one, i64 %h1, i64 %pick2\n  %high = lshr i64 %mixed, 29\n  %folded = xor i64 %mixed, %high\n  %hash = lshr i64 %folded, 1\n"
    );
    lines
}
