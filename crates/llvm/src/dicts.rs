//! Dicts and sets, in the runtime's blocks: lookups here, and anything that grows or copies one
//! in the runtime. A key's hash is worked out the way the runtime does it.

use std::fmt::Write as _;

use fennec_interp::embed::dicts::{DICT_HEADER_WORDS, ENTRY_WORDS, Word};
use fennec_interp::embed::layout::{TEXT, TEXT_BYTES, TEXT_CHARS};
use fennec_ir::{LocalId, Operand};
use fennec_layout::Ty;
use fennec_syntax::{BinOp, Span};

use crate::emit::Emit;
use crate::kinds::Kind;

const MIX: i64 = 0x9E37_79B9_7F4A_7C15_u64.cast_signed();
/// Where a dict's count of keys, entries used and slots less one are.
const KEYS: u32 = 16;
const USED: u32 = 24;
const MASK: u32 = 32;
/// Where an entry's key and value are.
const ENTRY_KEY: u32 = 8;
const ENTRY_VALUE: u32 = 16;
/// The hash a removed entry has.
const REMOVED: i64 = -1;

fn header_bytes() -> i64 {
    i64::try_from(DICT_HEADER_WORDS * 8).unwrap_or(40)
}

/// The start of a function that looks up a key: gives back its entry's address, or zero.
fn find_start(name: &str, key_type: &str) -> String {
    let header = header_bytes();
    format!(
        "define internal ptr @{name}(ptr %d, {key_type} %key, i64 %hash) #0 {{\nentry:\n  %mask_at = getelementptr inbounds i8, ptr %d, i64 {MASK}\n  %mask = load i64, ptr %mask_at\n  %slots = getelementptr inbounds i8, ptr %d, i64 {header}\n  %count = add i64 %mask, 1\n  %first = and i64 %hash, %mask\n  %entries = getelementptr inbounds i64, ptr %slots, i64 %count\n  br label %head\nhead:\n  %at = phi i64 [ %first, %entry ], [ %after, %next ]\n  %slot = getelementptr inbounds i64, ptr %slots, i64 %at\n  %taken = load i64, ptr %slot\n  %empty = icmp eq i64 %taken, 0\n  br i1 %empty, label %none, label %check\nnone:\n  ret ptr null\ncheck:\n  %number = add i64 %taken, -1\n  %entry_at = getelementptr inbounds [{ENTRY_WORDS} x i64], ptr %entries, i64 %number\n  %theirs = load i64, ptr %entry_at\n  %same_hash = icmp eq i64 %theirs, %hash\n  br i1 %same_hash, label %compare, label %next\nnext:\n  %stepped = add i64 %at, 1\n  %after = and i64 %stepped, %mask\n  br label %head\nfound:\n  ret ptr %entry_at\ncompare:\n  %other_at = getelementptr inbounds i8, ptr %entry_at, i64 {ENTRY_KEY}\n"
    )
}

impl Emit<'_, '_, '_> {
    fn word_kind(&self, word: Word) -> Option<Kind> {
        Kind::from_ty(self.module.layouts, word_ty(word))
    }

    /// A new empty dict with room for `room` keys.
    fn new_dict(&mut self, key: Word, value: Word, room: &str) -> String {
        let symbol = format!("d_{key:?}_{value:?}");
        self.module.shared.dicts.insert(symbol.clone(), (key, value));
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        self.declare("declare ptr @fennec_dict_new(i64, i64) nounwind");
        let (shape, dict) = (self.temp(), self.temp());
        self.line(&format!("{shape} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
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
                let mut hash = i64::from(words).wrapping_mul(MIX).to_string();
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
                let held = self.temp();
                self.line(&format!("{held} = mul i64 {key}, {MIX}"));
                held
            }
        };
        let (high, folded, done) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{high} = lshr i64 {mixed}, 29"));
        self.line(&format!("{folded} = xor i64 {mixed}, {high}"));
        self.line(&format!("{done} = lshr i64 {folded}, 1"));
        done
    }

    fn string_hash_helper(&mut self) {
        self.module.add_helper(
            "fen_text_hash",
            format!(
                "define internal i64 @fen_text_hash(ptr %text) #0 {{\nentry:\n  %length_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_BYTES}\n  %length = load i64, ptr %length_at\n  %start = mul i64 %length, {MIX}\n  %rounded = add i64 %length, 7\n  %words = lshr i64 %rounded, 3\n  %first = getelementptr inbounds i8, ptr %text, i64 {TEXT}\n  br label %head\nhead:\n  %hash = phi i64 [ %start, %entry ], [ %mixed, %body ]\n  %at = phi i64 [ 0, %entry ], [ %next, %body ]\n  %more = icmp ult i64 %at, %words\n  br i1 %more, label %body, label %done\nbody:\n  %place = getelementptr inbounds i64, ptr %first, i64 %at\n  %word = load i64, ptr %place\n  %turned = call i64 @llvm.fshl.i64(i64 %hash, i64 %hash, i64 23)\n  %xored = xor i64 %turned, %word\n  %mixed = mul i64 %xored, {MIX}\n  %next = add i64 %at, 1\n  br label %head\ndone:\n  ret i64 %hash\n}}\n\n"
            ),
        );
        self.declare("declare i64 @llvm.fshl.i64(i64, i64, i64)");
    }

    /// The address of a key's entry, or zero.
    fn find(&mut self, dict: &str, word: Word, key: &str) -> String {
        let hash = self.hash(word, key);
        let (name, key_type) = match word {
            Word::Str => {
                self.declare("declare i8 @fennec_box_equal(ptr, ptr) nounwind");
                let mut text = find_start("fen_find_text", "ptr");
                text += "  %other = load ptr, ptr %other_at\n  %same = icmp eq ptr %other, %key\n  br i1 %same, label %found, label %text\ntext:\n  %equal = call i8 @fennec_box_equal(ptr %other, ptr %key)\n  %is = icmp ne i8 %equal, 0\n  br i1 %is, label %found, label %next\n}\n\n";
                self.module.add_helper("fen_find_text", text);
                ("fen_find_text".to_owned(), "ptr")
            }
            Word::Plain(id) => {
                let words = self.module.layouts.get(id).map_or(0, |layout| layout.size / 8);
                let name = format!("fen_find_words{words}");
                let mut text = find_start(&name, "ptr");
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
                let mut text = find_start("fen_find_int", "i64");
                text += "  %other = load i64, ptr %other_at\n  %same = icmp eq i64 %other, %key\n  br i1 %same, label %found, label %next\n}\n\n";
                self.module.add_helper("fen_find_int", text);
                ("fen_find_int".to_owned(), "i64")
            }
        };
        let entry = self.temp();
        self.line(&format!("{entry} = call ptr @{name}(ptr {dict}, {key_type} {key}, i64 {hash})"));
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
        if matches!(word, Word::Float) {
            return Err("it uses a float as a key".to_owned());
        }
        Ok(value)
    }

    /// A value as the word a dict keeps, handing over an owner if it's a block.
    fn dict_word(&mut self, kind: &Kind, value: &str, retain: bool) -> String {
        let word = self.temp();
        match kind {
            Kind::Float => self.line(&format!("{word} = bitcast double {value} to i64")),
            Kind::Bool => self.line(&format!("{word} = zext i1 {value} to i64")),
            Kind::Str | Kind::Boxed(_) | Kind::List(_) | Kind::Dict(..) => {
                if retain {
                    self.retain(value);
                }
                self.line(&format!("{word} = ptrtoint ptr {value} to i64"));
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
                let block = self.key_block(id, key)?;
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
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let key = self.key_value(key, key_word)?;
        let held = match value {
            Some(value) => self.operand(value)?.0,
            None => String::new(),
        };
        self.declare("declare ptr @fennec_dict_set(ptr, i64, i64) nounwind");
        let (dict, count, alone) = (self.temp(), self.temp(), self.temp());
        let (quick, change, slow, done) =
            (self.label("quick"), self.label("change"), self.label("slow"), self.label("done"));
        self.line(&format!("{dict} = load ptr, ptr {place}"));
        self.line(&format!("{count} = load i64, ptr {dict}"));
        self.line(&format!("{alone} = icmp eq i64 {count}, 1"));
        self.line(&format!("br i1 {alone}, label %{quick}, label %{slow}"));
        self.start_block(&quick);
        // With the dict alone, it's the same block the lookup searched, unchanged since.
        let entry = match found {
            Some(entry) => entry,
            None => self.find(&dict, key_word, &key),
        };
        let missing = self.temp();
        self.line(&format!("{missing} = icmp eq ptr {entry}, null"));
        self.line(&format!("br i1 {missing}, label %{slow}, label %{change}"));
        self.start_block(&change);
        let value_at = self.temp();
        self.line(&format!("{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {ENTRY_VALUE}"));
        if value_kind.counted() {
            self.retain(&held);
            let old = self.temp();
            self.line(&format!("{old} = load ptr, ptr {value_at}"));
            self.line(&format!("store ptr {held}, ptr {value_at}"));
            self.release(&old);
        } else if value_kind != Kind::Unit {
            self.store_field(&value_kind, &held, &value_at, "");
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        let key_held = self.key_word(key_word, &key)?;
        let value_held = self.dict_word(&value_kind, &held, true);
        let changed = self.temp();
        self.line(&format!(
            "{changed} = call ptr @fennec_dict_set(ptr {dict}, i64 {key_held}, i64 {value_held})"
        ));
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
        self.declare("declare void @fennec_dict_missing(ptr, i64, i8, i64, i64) #1");
        let shown = self.key_word(key_word, &key)?;
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!(
            "call void @fennec_dict_missing(ptr {dict}, i64 {shown}, i8 0, i64 {from}, i64 {to})"
        ));
        let failed = self.failed_exit();
        self.line(&failed);
        self.start_block(&found);
        let value_at = self.temp();
        self.line(&format!("{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {ENTRY_VALUE}"));
        self.read_into(dest, &value_kind, &value_at, "")
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
        if matches!(optional, Kind::Maybe(_)) {
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
            self.line(&format!("{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {ENTRY_VALUE}"));
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
        let (slot, memory) = (self.slot(dest), optional.memory(self.module.layouts));
        self.line(&format!("store {memory} zeroinitializer, ptr {slot}"));
        self.line(&format!("store i64 {number}, ptr {slot}"));
        let value_at = self.temp();
        self.line(&format!("{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {ENTRY_VALUE}"));
        let value = self.load_field(&value_kind, &value_at, "");
        if value_kind.counted() {
            self.retain_value(&value, &value_kind);
        }
        let at = self.offset(&slot, offset);
        self.store_field(&value_kind, &value, &at, "");
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
    /// right here; other text asks the runtime.
    fn slice_value_at(&mut self, dict: &str, [text, from, to]: &[String; 3]) -> String {
        self.string_slice_helpers();
        let (bytes_at, chars_at) =
            (self.offset(text, TEXT_BYTES.unsigned_abs()), self.offset(text, TEXT_CHARS.unsigned_abs()));
        let (length, chars, ascii) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{length} = load i64, ptr {bytes_at}"));
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{ascii} = icmp eq i64 {length}, {chars}"));
        let (here, there, joined) = (self.label("ascii"), self.label("other"), self.label("looked"));
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
        let (mixed, high, folded, hash, entry, none, value_place, found) = (
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
        );
        self.line(&format!("{mixed} = call i64 @fen_slice_hash(ptr {first}, i64 {size})"));
        self.line(&format!("{high} = lshr i64 {mixed}, 29"));
        self.line(&format!("{folded} = xor i64 {mixed}, {high}"));
        self.line(&format!("{hash} = lshr i64 {folded}, 1"));
        self.line(&format!(
            "{entry} = call ptr @fen_find_slice(ptr {dict}, ptr {first}, i64 {size}, i64 {hash})"
        ));
        self.line(&format!("{none} = icmp eq ptr {entry}, null"));
        self.line(&format!("{value_place} = getelementptr inbounds i8, ptr {entry}, i64 {ENTRY_VALUE}"));
        self.line(&format!("{found} = select i1 {none}, ptr null, ptr {value_place}"));
        let here_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&there);
        self.declare("declare ptr @fennec_dict_find_slice(ptr, ptr, i64, i64) nounwind");
        let asked = self.temp();
        self.line(&format!(
            "{asked} = call ptr @fennec_dict_find_slice(ptr {dict}, ptr {text}, i64 {from}, i64 {to})"
        ));
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let value_at = self.temp();
        self.line(&format!("{value_at} = phi ptr [ {found}, %{here_end} ], [ {asked}, %{there} ]"));
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
                "define internal i64 @fen_slice_hash(ptr %bytes, i64 %length) #0 {{\nentry:\n  %part = alloca i64\n  %start = mul i64 %length, {MIX}\n  %whole = lshr i64 %length, 3\n  br label %head\nhead:\n  %hash = phi i64 [ %start, %entry ], [ %mixed, %body ]\n  %at = phi i64 [ 0, %entry ], [ %next, %body ]\n  %more = icmp ult i64 %at, %whole\n  br i1 %more, label %body, label %tail\nbody:\n  %place = getelementptr inbounds i64, ptr %bytes, i64 %at\n  %word = load i64, ptr %place, align 1\n  %turned = call i64 @llvm.fshl.i64(i64 %hash, i64 %hash, i64 23)\n  %xored = xor i64 %turned, %word\n  %mixed = mul i64 %xored, {MIX}\n  %next = add i64 %at, 1\n  br label %head\ntail:\n  %rest = and i64 %length, 7\n  %some = icmp ne i64 %rest, 0\n  br i1 %some, label %last, label %done\nlast:\n  store i64 0, ptr %part\n  %from = getelementptr inbounds i64, ptr %bytes, i64 %whole\n  call void @llvm.memcpy.p0.p0.i64(ptr %part, ptr %from, i64 %rest, i1 false)\n  %last_word = load i64, ptr %part\n  %last_turned = call i64 @llvm.fshl.i64(i64 %hash, i64 %hash, i64 23)\n  %last_xored = xor i64 %last_turned, %last_word\n  %last_mixed = mul i64 %last_xored, {MIX}\n  ret i64 %last_mixed\ndone:\n  ret i64 %hash\n}}\n\n"
            ),
        );
        let mut find = find_start("fen_find_slice", "ptr")
            .replace("(ptr %d, ptr %key, i64 %hash)", "(ptr %d, ptr %key, i64 %length, i64 %hash)");
        find += &format!(
            "  %other = load ptr, ptr %other_at\n  %their_length_at = getelementptr inbounds i8, ptr %other, i64 {TEXT_BYTES}\n  %their_length = load i64, ptr %their_length_at\n  %same_length = icmp eq i64 %their_length, %length\n  br i1 %same_length, label %bytes, label %next\nbytes:\n  %their_text = getelementptr inbounds i8, ptr %other, i64 {TEXT}\n  %order = call i32 @memcmp(ptr %their_text, ptr %key, i64 %length)\n  %equal = icmp eq i32 %order, 0\n  br i1 %equal, label %found, label %next\n}}\n\n"
        );
        self.module.add_helper("fen_find_slice", find);
    }

    /// `dict.get(text[a:b])`, finding those bytes where they are.
    fn slice_get(
        &mut self,
        dest: LocalId,
        dict: &Operand,
        key: &crate::analysis::SliceKey,
    ) -> Result<(), String> {
        let (dict, kind) = self.operand(dict)?;
        let Kind::Dict(_, value_word) = kind else { return Err("`dict.get` on something else".to_owned()) };
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let place = self.slice_place(key)?;
        let value_at = self.slice_value_at(&dict, &place);
        let missing = self.temp();
        self.line(&format!("{missing} = icmp eq ptr {value_at}, null"));
        let (absent, found, done) = (self.label("absent"), self.label("found"), self.label("done"));
        self.line(&format!("br i1 {missing}, label %{absent}, label %{found}"));
        self.start_block(&absent);
        self.copy(dest, &Operand::None)?;
        self.line(&format!("br label %{done}"));
        self.start_block(&found);
        let optional = self.kind_of(dest)?.clone();
        if matches!(optional, Kind::Maybe(_)) {
            self.read_into(dest, &value_kind, &value_at, "")?;
        } else {
            let (number, fields) = self.case_of(&optional, "Some")?;
            let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
            let (slot, memory) = (self.slot(dest), optional.memory(self.module.layouts));
            self.line(&format!("store {memory} zeroinitializer, ptr {slot}"));
            self.line(&format!("store i64 {number}, ptr {slot}"));
            let value = self.load_field(&value_kind, &value_at, "");
            if value_kind.counted() {
                self.retain_value(&value, &value_kind);
            }
            let at = self.offset(&slot, offset);
            self.store_field(&value_kind, &value, &at, "");
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// `dict[text[a:b]] = value`, a key block made only when the key is new.
    pub(crate) fn slice_set(
        &mut self,
        place: &str,
        value_word: Word,
        key: &crate::analysis::SliceKey,
        value: &Operand,
    ) -> Result<(), String> {
        let value_kind = self.word_kind(value_word).ok_or("a dict value this backend doesn't take yet")?;
        let (held, fresh) = self.value_for(value, &value_kind)?;
        let sliced = self.slice_place(key)?;
        let [text, from, to] = sliced.clone();
        let (dict, count) = (self.temp(), self.temp());
        self.line(&format!("{dict} = load ptr, ptr {place}"));
        // A key that's there, in a dict nothing else holds, just gets its new value.
        let value_at = self.slice_value_at(&dict, &sliced);
        self.line(&format!("{count} = load i64, ptr {dict}"));
        let (alone, there, quick) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{alone} = icmp eq i64 {count}, 1"));
        self.line(&format!("{there} = icmp ne ptr {value_at}, null"));
        self.line(&format!("{quick} = and i1 {alone}, {there}"));
        let (change, add, done) = (self.label("change"), self.label("add"), self.label("stored"));
        self.line(&format!("br i1 {quick}, label %{change}, label %{add}"));
        self.start_block(&change);
        if value_kind.counted() {
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
        let word = self.dict_word(&value_kind, &held, !fresh);
        self.declare("declare ptr @fennec_dict_add_slice(ptr, ptr, i64, i64, i64) nounwind");
        let changed = self.temp();
        self.line(&format!(
            "{changed} = call ptr @fennec_dict_add_slice(ptr {dict}, ptr {text}, i64 {from}, i64 {to}, i64 {word})"
        ));
        self.line(&format!("store ptr {changed}, ptr {place}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// `dict.pop(key)`: the runtime takes the key out, failing as the interpreter does if it
    /// isn't there, and hands back its value with its owner.
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
        let key_held = self.key_word(key_word, &key)?;
        self.declare("declare ptr @fennec_dict_pop(ptr, i64, i64, i64) nounwind");
        self.declare("declare i64 @fennec_dict_popped() nounwind");
        let place = self.slot(*dict);
        let (held, changed, word) = (self.temp(), self.temp(), self.temp());
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!("{held} = load ptr, ptr {place}"));
        self.line(&format!(
            "{changed} = call ptr @fennec_dict_pop(ptr {held}, i64 {key_held}, i64 {from}, i64 {to})"
        ));
        self.line(&format!("store ptr {changed}, ptr {place}"));
        self.leave_if_failed();
        self.line(&format!("{word} = call i64 @fennec_dict_popped()"));
        // The value's word, read back as what it holds, already with its owner.
        let at = self.place_for(&Kind::Int);
        self.line(&format!("store i64 {word}, ptr {at}"));
        let value = self.load_field(&value_kind, &at, "");
        match dest {
            Some(dest) if self.kind_of(dest)? == &value_kind => self.assign(dest, &value),
            Some(dest) => {
                // Given where an optional's wanted.
                let optional = self.kind_of(dest)?.clone();
                if matches!(optional, Kind::Maybe(_)) {
                    return self.assign(dest, &value);
                }
                let (number, fields) = self.case_of(&optional, "Some")?;
                let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
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
            return self.slice_get(dest, dict, &sliced);
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
        self.line(&format!("{value_at} = getelementptr inbounds i8, ptr {entry}, i64 {ENTRY_VALUE}"));
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
            "{entry_at} = getelementptr inbounds [{ENTRY_WORDS} x i64], ptr {entry}, i64 {at}"
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
        self.line(&format!("{value_at} = getelementptr inbounds i8, ptr {entry_at}, i64 {ENTRY_VALUE}"));
        let key_value = self.dict_part(&key_kind, &key_at);
        let value_value = self.dict_part(&value_kind, &value_at);
        match what {
            "keys" => self.put_item(&item_kind, &key_kind, &key_value, &place),
            "values" => self.put_item(&item_kind, &value_kind, &value_value, &place),
            _ => self.put_pair(&item_kind, &place, [(&key_kind, &key_value), (&value_kind, &value_value)])?,
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
        Word::Bool => Ty::Bool,
        Word::Str => Ty::Str,
        Word::Nothing => Ty::Unit,
        Word::Plain(id) => Ty::Struct(id),
    }
}
