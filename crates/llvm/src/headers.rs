//! A string held as its 24-byte header, when the layouts say so, as a program's always do: up to
//! 23 bytes in the header itself, or a block's address and its length. Byte 23 says which: its
//! top bit is set for an inline string, whose length is in the rest of it, and clear for a
//! block. A header is a `[3 x i64]` value. The runtime's string calls still take and give
//! blocks, so a header crosses as a block and comes back as a header.

use std::fmt::Write as _;

use fennec_syntax::BinOp;

use crate::emit::Emit;
use crate::kinds::Kind;

/// How LLVM holds a header.
pub(crate) const HEADER: &str = "[3 x i64]";

/// The most bytes a header holds itself.
const INLINE_BYTES: usize = 23;

/// A literal's header when it fits inline: its bytes, zero after the end, and `0x80 | length`
/// in byte 23.
fn inline_words(text: &str) -> Option<[u64; 3]> {
    let bytes = text.as_bytes();
    if bytes.len() > INLINE_BYTES {
        return None;
    }
    let mut packed = [0_u8; 24];
    packed.get_mut(..bytes.len())?.copy_from_slice(bytes);
    *packed.last_mut()? = 0x80 | u8::try_from(bytes.len()).ok()?;
    let word = |at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(packed.get(at * 8..at * 8 + 8)?.try_into().ok()?))
    };
    Some([word(0)?, word(1)?, word(2)?])
}

impl Emit<'_, '_, '_> {
    /// Whether strings are held as headers.
    pub(crate) fn headers(&self) -> bool {
        self.module.layouts.text_headers()
    }

    /// One word of a header.
    pub(crate) fn header_word(&mut self, header: &str, at: u32) -> String {
        let word = self.temp();
        self.line(&format!("{word} = extractvalue {HEADER} {header}, {at}"));
        word
    }

    /// A header of three words.
    pub(crate) fn header_of(&mut self, words: [&str; 3]) -> String {
        let mut header = "poison".to_owned();
        for (at, word) in words.iter().enumerate() {
            let next = self.temp();
            self.line(&format!("{next} = insertvalue {HEADER} {header}, i64 {word}, {at}"));
            header = next;
        }
        header
    }

    /// Whether a header holds a block: byte 23's top bit is clear.
    pub(crate) fn header_in_block(&mut self, header: &str) -> String {
        let last = self.header_word(header, 2);
        let in_block = self.temp();
        self.line(&format!("{in_block} = icmp sgt i64 {last}, -1"));
        in_block
    }

    /// The block a header holds, or null for an inline one.
    fn header_block(&mut self, header: &str) -> String {
        let in_block = self.header_in_block(header);
        let first = self.header_word(header, 0);
        let (word, block) = (self.temp(), self.temp());
        self.line(&format!("{word} = select i1 {in_block}, i64 {first}, i64 0"));
        self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
        block
    }

    /// One more owner for what a header holds: its block, if it has one.
    pub(crate) fn retain_text(&mut self, header: &str) {
        let block = self.header_block(header);
        let (none, held, done) = (self.temp(), self.label("text_held"), self.label("text_kept"));
        self.line(&format!("{none} = icmp eq ptr {block}, null"));
        self.line(&format!("br i1 {none}, label %{done}, label %{held}"));
        self.start_block(&held);
        self.retain(&block);
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// One owner fewer for what a header holds.
    pub(crate) fn release_text(&mut self, header: &str) {
        let block = self.header_block(header);
        self.release(&block);
    }

    /// A literal's header: inline when it fits, and otherwise its block, which lives forever.
    pub(crate) fn literal_header(&mut self, text: &str) -> String {
        if let Some([a, b, c]) = inline_words(text) {
            let [a, b, c] = [a, b, c].map(u64::cast_signed);
            return format!("[i64 {a}, i64 {b}, i64 {c}]");
        }
        let block = self.literal(text);
        let word = self.temp();
        self.line(&format!("{word} = ptrtoint ptr {block} to i64"));
        let length = text.len().to_string();
        self.header_of([&word, &length, "0"])
    }

    /// A string's block for a runtime call that takes one, and whether it was made for the call
    /// (an inline string's), so [`Self::let_go_text_block`] lets go of it after.
    pub(crate) fn text_block(&mut self, header: &str) -> (String, String) {
        let in_block = self.header_in_block(header);
        let (made, held, done) =
            (self.label("text_made"), self.label("text_block"), self.label("text_ready"));
        self.line(&format!("br i1 {in_block}, label %{held}, label %{made}"));
        self.start_block(&held);
        let first = self.header_word(header, 0);
        let kept = self.temp();
        self.line(&format!("{kept} = inttoptr i64 {first} to ptr"));
        let from_held = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&made);
        self.declare("declare ptr @fennec_text_block(i64, i64, i64) nounwind");
        let [a, b, c] = [0, 1, 2].map(|at| self.header_word(header, at));
        let new = self.temp();
        self.line(&format!("{new} = call ptr @fennec_text_block(i64 {a}, i64 {b}, i64 {c})"));
        let from_made = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let (block, owned) = (self.temp(), self.temp());
        self.line(&format!("{block} = phi ptr [ {kept}, %{from_held} ], [ {new}, %{from_made} ]"));
        self.line(&format!("{owned} = phi i1 [ false, %{from_held} ], [ true, %{from_made} ]"));
        (block, owned)
    }

    /// Lets go of a block [`Self::text_block`] made for a call.
    pub(crate) fn let_go_text_block(&mut self, block: &str, owned: &str) {
        let (made, done) = (self.label("text_made_gone"), self.label("text_gone"));
        self.line(&format!("br i1 {owned}, label %{made}, label %{done}"));
        self.start_block(&made);
        self.release(block);
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// A string's block with an owner of its own, for something that holds strings as blocks:
    /// a dict, or an optional.
    pub(crate) fn owned_text_block(&mut self, header: &str) -> String {
        self.declare("declare ptr @fennec_text_block(i64, i64, i64) nounwind");
        let [a, b, c] = [0, 1, 2].map(|at| self.header_word(header, at));
        let block = self.temp();
        self.line(&format!("{block} = call ptr @fennec_text_block(i64 {a}, i64 {b}, i64 {c})"));
        block
    }

    /// The header for a string's block, taking over its owner: a short string comes back inline
    /// and its block is let go of.
    pub(crate) fn text_from_block(&mut self, block: &str) -> String {
        self.declare(&format!("declare void @fennec_text_of(ptr sret({HEADER}) align 8, ptr) nounwind"));
        let place = self.place_for_text();
        self.line(&format!("call void @fennec_text_of(ptr sret({HEADER}) align 8 {place}, ptr {block})"));
        let header = self.temp();
        self.line(&format!("{header} = load {HEADER}, ptr {place}"));
        header
    }

    /// The header for a string's block, which keeps its own owner.
    pub(crate) fn text_from_borrowed_block(&mut self, block: &str) -> String {
        self.retain(block);
        self.text_from_block(block)
    }

    /// A slot on this function's stack for a header.
    pub(crate) fn place_for_text(&mut self) -> String {
        let at = self.label("%text");
        let _ = writeln!(self.allocas, "  {at} = alloca {HEADER}");
        at
    }

    /// `part in text` for two headers, which makes no block for either.
    pub(crate) fn headers_contain(&mut self, part: &str, text: &str) -> String {
        self.declare("declare i8 @fennec_text_contains(i64, i64, i64, i64, i64, i64) nounwind");
        let [t0, t1, t2] = [0, 1, 2].map(|at| self.header_word(text, at));
        let [p0, p1, p2] = [0, 1, 2].map(|at| self.header_word(part, at));
        let (found, bit) = (self.temp(), self.temp());
        self.line(&format!(
            "{found} = call i8 @fennec_text_contains(i64 {t0}, i64 {t1}, i64 {t2}, i64 {p0}, i64 {p1}, i64 {p2})"
        ));
        self.line(&format!("{bit} = trunc i8 {found} to i1"));
        bit
    }

    /// Runs `body` with strings as blocks: each header given as its block, or a block made for
    /// it when it's inline, which is let go of after. Without headers, the values are blocks.
    pub(crate) fn on_blocks<const N: usize, R>(
        &mut self,
        values: [&str; N],
        body: impl FnOnce(&mut Self, [String; N]) -> R,
    ) -> R {
        if !self.headers() {
            return body(self, values.map(str::to_owned));
        }
        let made = values.map(|value| self.text_block(value));
        let result = body(self, made.clone().map(|(block, _)| block));
        for (block, owned) in &made {
            self.let_go_text_block(block, owned);
        }
        result
    }

    /// A string's new block, owned once, as a string value: its header, taking over the owner,
    /// or the block itself without headers.
    pub(crate) fn text_value(&mut self, block: &str) -> String {
        if self.headers() { self.text_from_block(block) } else { block.to_owned() }
    }

    /// Whether two headers hold the same text. Equal strings always have the same form, so two
    /// inline ones are equal exactly when their words are, and only two blocks need comparing.
    pub(crate) fn same_headers(&mut self, a: &str, b: &str) -> String {
        // A literal that fits is inline, so only an inline string with the same words equals it.
        if a.starts_with("[i64") || b.starts_with("[i64") {
            let mut same = "true".to_owned();
            for at in 0..3 {
                let (x, y) = (self.header_word(a, at), self.header_word(b, at));
                let (equal, all) = (self.temp(), self.temp());
                self.line(&format!("{equal} = icmp eq i64 {x}, {y}"));
                self.line(&format!("{all} = and i1 {same}, {equal}"));
                same = all;
            }
            return same;
        }
        let (a_block, b_block) = (self.header_in_block(a), self.header_in_block(b));
        let both = self.temp();
        self.line(&format!("{both} = and i1 {a_block}, {b_block}"));
        let (blocks, words, done) =
            (self.label("text_blocks"), self.label("text_words"), self.label("text_compared"));
        self.line(&format!("br i1 {both}, label %{blocks}, label %{words}"));
        self.start_block(&words);
        let mut same = "true".to_owned();
        for at in 0..3 {
            let (x, y, equal, all) =
                (self.header_word(a, at), self.header_word(b, at), self.temp(), self.temp());
            self.line(&format!("{equal} = icmp eq i64 {x}, {y}"));
            self.line(&format!("{all} = and i1 {same}, {equal}"));
            same = all;
        }
        let words_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&blocks);
        let (x, y) = (self.header_word(a, 0), self.header_word(b, 0));
        let (x_block, y_block) = (self.temp(), self.temp());
        self.line(&format!("{x_block} = inttoptr i64 {x} to ptr"));
        self.line(&format!("{y_block} = inttoptr i64 {y} to ptr"));
        let compared = self.same_text(BinOp::Eq, (&x_block, &y_block));
        let blocks_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!("{result} = phi i1 [ {same}, %{words_end} ], [ {compared}, %{blocks_end} ]"));
        result
    }

    /// An operator on two strings held as headers.
    pub(crate) fn header_binary(&mut self, op: BinOp, (a, b): (&str, &str)) -> Result<String, String> {
        match op {
            BinOp::Eq | BinOp::NotEq => {
                let same = self.same_headers(a, b);
                if op == BinOp::Eq {
                    return Ok(same);
                }
                let differs = self.temp();
                self.line(&format!("{differs} = xor i1 {same}, true"));
                Ok(differs)
            }
            BinOp::Add => Ok(self.header_concat(a, b)),
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                let cc = match op {
                    BinOp::Lt => "slt",
                    BinOp::LtEq => "sle",
                    BinOp::Gt => "sgt",
                    _ => "sge",
                };
                let (ordering, result) = (self.header_order(a, b), self.temp());
                self.line(&format!("{result} = icmp {cc} i64 {ordering}, 0"));
                Ok(result)
            }
            _ => self.on_blocks([a, b], |emit, [x, y]| emit.string_binary(op, (&x, &y))),
        }
    }

    /// A helper ordering two headers in memory, by their texts, as -1, 0 or 1.
    pub(crate) fn header_order_helper(&mut self) {
        self.declare("declare ptr @fennec_text_block(i64, i64, i64) nounwind");
        self.declare("declare i64 @fennec_str_compare(ptr, ptr) nounwind");
        self.declare("declare void @fennec_free(ptr) nounwind");
        let mut text = "define internal i64 @fen_header_order(ptr %x, ptr %y) #0 {\nentry:\n".to_owned();
        for side in ["x", "y"] {
            for at in 0..3 {
                text += &format!(
                    "  %{side}p{at} = getelementptr inbounds i64, ptr %{side}, i64 {at}\n  %{side}w{at} = load i64, ptr %{side}p{at}\n"
                );
            }
            text += &format!(
                "  %{side}b = call ptr @fennec_text_block(i64 %{side}w0, i64 %{side}w1, i64 %{side}w2)\n"
            );
        }
        text += "  %o = call i64 @fennec_str_compare(ptr %xb, ptr %yb)\n";
        for side in ["x", "y"] {
            text += &format!(
                "  %{side}z = call i1 @fen_owner_gone(ptr %{side}b)\n  br i1 %{side}z, label %{side}free, label %{side}kept\n{side}free:\n  call void @fennec_free(ptr %{side}b)\n  br label %{side}kept\n{side}kept:\n"
            );
        }
        text += "  ret i64 %o\n}\n\n";
        self.owner_gone_helper();
        self.module.add_helper("fen_header_order", text);
    }

    /// A value from where something holds a string as a block, as a string value: a header
    /// with an owner of its own (the third half says it has one), or the value as it is.
    pub(crate) fn as_header(&mut self, kind: &Kind, value: &str) -> (Kind, String, bool) {
        if *kind == Kind::TextBlock && self.headers() {
            return (Kind::Str, self.text_from_borrowed_block(value), true);
        }
        (kind.clone(), value.to_owned(), false)
    }

    /// `a == b` or `a != b` where a side may be an optional string's header, which is all zero for
    /// `None`: two `None`s are the same, a `None` is like no string, and two strings compare as
    /// strings. A `None`'s zero words read as a block at address 0, so the texts are compared only
    /// when neither side is one.
    pub(crate) fn maybe_text_binary(
        &mut self,
        op: BinOp,
        (a, a_optional): (&str, bool),
        (b, b_optional): (&str, bool),
    ) -> String {
        let a_none = if a_optional { self.header_is_none(a) } else { "false".to_owned() };
        let b_none = if b_optional { self.header_is_none(b) } else { "false".to_owned() };
        let (both, either) = (self.temp(), self.temp());
        self.line(&format!("{both} = and i1 {a_none}, {b_none}"));
        self.line(&format!("{either} = or i1 {a_none}, {b_none}"));
        let (texts, done) = (self.label("maybe_texts"), self.label("maybe_compared"));
        let start = self.current.clone();
        self.line(&format!("br i1 {either}, label %{done}, label %{texts}"));
        self.start_block(&texts);
        let same = self.same_headers(a, b);
        let texts_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let equal = self.temp();
        self.line(&format!("{equal} = phi i1 [ {both}, %{start} ], [ {same}, %{texts_end} ]"));
        if op == BinOp::Eq {
            return equal;
        }
        let differs = self.temp();
        self.line(&format!("{differs} = xor i1 {equal}, true"));
        differs
    }

    /// Whether an optional string's header is `None`: all zero, which no string is.
    pub(crate) fn header_is_none(&mut self, header: &str) -> String {
        let (first, last) = (self.header_word(header, 0), self.header_word(header, 2));
        let (both, none) = (self.temp(), self.temp());
        self.line(&format!("{both} = or i64 {first}, {last}"));
        self.line(&format!("{none} = icmp eq i64 {both}, 0"));
        none
    }

    /// An inline header's length in bytes: what byte 23 holds below its top bit.
    fn inline_length(&mut self, header: &str) -> String {
        let last = self.header_word(header, 2);
        let (shifted, length) = (self.temp(), self.temp());
        self.line(&format!("{shifted} = lshr i64 {last}, 56"));
        self.line(&format!("{length} = and i64 {shifted}, 127"));
        length
    }

    /// `a + b`: done in place in the words when both are inline and the result fits, and by
    /// the runtime otherwise.
    fn header_concat(&mut self, a: &str, b: &str) -> String {
        let (a_block, b_block) = (self.header_in_block(a), self.header_in_block(b));
        let (a_length, b_length) = (self.inline_length(a), self.inline_length(b));
        let (either, total, fits, quick) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{either} = or i1 {a_block}, {b_block}"));
        self.line(&format!("{total} = add i64 {a_length}, {b_length}"));
        self.line(&format!("{fits} = icmp ule i64 {total}, 23"));
        let inline = self.temp();
        self.line(&format!("{inline} = xor i1 {either}, true"));
        self.line(&format!("{quick} = and i1 {inline}, {fits}"));
        let (words, runtime, done) =
            (self.label("concat_words"), self.label("concat_runtime"), self.label("concat_done"));
        self.line(&format!("br i1 {quick}, label %{words}, label %{runtime}"));
        self.start_block(&words);
        let (from_a, from_b, out) = (self.place_for_text(), self.place_for_text(), self.place_for_text());
        self.line(&format!("store {HEADER} {a}, ptr {from_a}"));
        self.line(&format!("store {HEADER} {b}, ptr {from_b}"));
        self.line(&format!("store {HEADER} zeroinitializer, ptr {out}"));
        self.small_copy(&out, &from_a, &a_length);
        let after = self.temp();
        self.line(&format!("{after} = getelementptr inbounds i8, ptr {out}, i64 {a_length}"));
        self.small_copy(&after, &from_b, &b_length);
        let (tag, tag_at) = (self.temp(), self.temp());
        self.line(&format!("{tag} = or i64 {total}, 128"));
        self.line(&format!("{tag_at} = getelementptr inbounds i8, ptr {out}, i64 23"));
        let byte = self.temp();
        self.line(&format!("{byte} = trunc i64 {tag} to i8"));
        self.line(&format!("store i8 {byte}, ptr {tag_at}"));
        let joined_words = self.temp();
        self.line(&format!("{joined_words} = load {HEADER}, ptr {out}"));
        let words_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&runtime);
        let joined = self.on_blocks([a, b], |emit, [x, y]| {
            emit.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
            let joined = emit.temp();
            emit.line(&format!("{joined} = call ptr @fennec_str_concat(ptr {x}, ptr {y})"));
            joined
        });
        let joined = self.text_from_block(&joined);
        let runtime_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!(
            "{result} = phi {HEADER} [ {joined_words}, %{words_end} ], [ {joined}, %{runtime_end} ]"
        ));
        result
    }

    /// `text += more` on a local that owns its string: a string in a block grows in place when
    /// nothing else holds it and it has room, as without headers; anything else is joined.
    pub(crate) fn header_append(&mut self, dest: fennec_ir::LocalId, a: &str, b: &str) -> Result<(), String> {
        let in_block = self.header_in_block(a);
        let (grow, join, done) =
            (self.label("append_grow"), self.label("append_join"), self.label("append_done"));
        self.line(&format!("br i1 {in_block}, label %{grow}, label %{join}"));
        self.start_block(&grow);
        let first = self.header_word(a, 0);
        let block = self.temp();
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        let grown = self.append_header(&block, b);
        let (length_at, length, address) = (self.temp(), self.temp(), self.temp());
        self.line(&format!(
            "{length_at} = getelementptr inbounds i8, ptr {grown}, i64 {}",
            fennec_rt::embed::layout::TEXT_BYTES
        ));
        self.line(&format!("{length} = load i64, ptr {length_at}"));
        self.line(&format!("{address} = ptrtoint ptr {grown} to i64"));
        let header = self.header_of([&address, &length, "0"]);
        let slot = self.slot(dest);
        self.line(&format!("store {HEADER} {header}, ptr {slot}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&join);
        let joined = self.header_concat(a, b);
        self.assign(dest, &joined)?;
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// `text_plain_end(text, at)` and `text_escape_at(text, at)`: from byte `at`, the first byte a
    /// JSON reader or writer has to look at, or the text's length. Reading, that's `"`, `\\` or a
    /// control character; writing, it's those, DEL, and any 194, which starts the C1 controls.
    /// Each is one tight loop over the bytes where they are.
    pub(crate) fn text_scan(
        &mut self,
        dest: fennec_ir::LocalId,
        name: &str,
        args: &[fennec_ir::Operand],
    ) -> Result<(), String> {
        if !self.headers() {
            return Err(format!("it calls `{name}` where strings aren't headers"));
        }
        let (text, _) = self.operand(args.first().ok_or("no text")?)?;
        let (from, _) = self.operand(args.get(1).ok_or("no place")?)?;
        let length = self.header_byte_length(&text);
        let bytes = self.header_bytes(&text);
        let before = self.current.clone();
        let (head, body, done) = (self.label("scan"), self.label("scan_byte"), self.label("scanned"));
        let (at, more, next) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("br label %{head}"));
        self.start_block(&head);
        self.line(&format!("{at} = phi i64 [ {from}, %{before} ], [ {next}, %{body} ]"));
        self.line(&format!("{more} = icmp ult i64 {at}, {length}"));
        let check = self.label("scan_check");
        self.line(&format!("br i1 {more}, label %{check}, label %{done}"));
        self.start_block(&check);
        let (place, byte, control, quote, slash, special) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{place} = getelementptr inbounds i8, ptr {bytes}, i64 {at}"));
        self.line(&format!("{byte} = load i8, ptr {place}"));
        self.line(&format!("{control} = icmp ult i8 {byte}, 32"));
        self.line(&format!("{quote} = icmp eq i8 {byte}, 34"));
        self.line(&format!("{slash} = icmp eq i8 {byte}, 92"));
        let (some, either) = (self.temp(), self.temp());
        self.line(&format!("{some} = or i1 {control}, {quote}"));
        self.line(&format!("{either} = or i1 {some}, {slash}"));
        if name == "intrinsics.text_escape_at" {
            let (delete, wide, more_special) = (self.temp(), self.temp(), self.temp());
            self.line(&format!("{delete} = icmp eq i8 {byte}, 127"));
            self.line(&format!("{wide} = icmp eq i8 {byte}, -62"));
            self.line(&format!("{more_special} = or i1 {delete}, {wide}"));
            self.line(&format!("{special} = or i1 {either}, {more_special}"));
        } else {
            self.line(&format!("{special} = or i1 {either}, false"));
        }
        self.line(&format!("br i1 {special}, label %{done}, label %{body}"));
        self.start_block(&body);
        self.line(&format!("{next} = add i64 {at}, 1"));
        self.line(&format!("br label %{head}"));
        self.start_block(&done);
        self.assign(dest, &at)
    }

    /// Where a header's text starts: in its block, or in a copy of the header on the stack.
    pub(crate) fn header_bytes(&mut self, header: &str) -> String {
        let in_block = self.header_in_block(header);
        let place = self.place_for_text();
        self.line(&format!("store {HEADER} {header}, ptr {place}"));
        let first = self.header_word(header, 0);
        let (block, text, bytes) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        self.line(&format!("{text} = getelementptr i8, ptr {block}, i64 {}", crate::emit::TEXT_START));
        self.line(&format!("{bytes} = select i1 {in_block}, ptr {text}, ptr {place}"));
        bytes
    }

    /// A header's length in bytes, from its block or from byte 23.
    pub(crate) fn header_byte_length(&mut self, header: &str) -> String {
        let in_block = self.header_in_block(header);
        let (inline, held) = (self.inline_length(header), self.header_word(header, 1));
        let length = self.temp();
        self.line(&format!("{length} = select i1 {in_block}, i64 {held}, i64 {inline}"));
        length
    }

    /// `str.byte_len` and `str.char_at_byte` on a header: a character is made inline from its
    /// bytes, with no block and nothing to count.
    pub(crate) fn header_by_bytes(
        &mut self,
        dest: fennec_ir::LocalId,
        name: &str,
        args: &[fennec_ir::Operand],
        text: &str,
    ) -> Result<(), String> {
        if name == "str.byte_len" {
            let length = self.header_byte_length(text);
            return self.assign(dest, &length);
        }
        if name == "str.is_ascii" {
            let ascii = self.header_ascii(text);
            return self.assign(dest, &ascii);
        }
        let (byte, _) = self.operand(args.get(1).ok_or("no argument")?)?;
        // Whether the whole text is ASCII, which a loop over it works out once: each character is
        // then its byte, with nothing to test.
        let whole_ascii = match args.get(2) {
            Some(ascii) => self.operand(ascii)?.0,
            None => self.header_ascii(text),
        };
        let start = self.header_bytes(text);
        let lead_at = self.temp();
        self.line(&format!("{lead_at} = getelementptr inbounds i8, ptr {start}, i64 {byte}"));
        let header = self.char_header_at(&lead_at, Some(&whole_ascii));
        self.assign(dest, &header)
    }

    /// The character whose first byte is at `lead_at`, as an inline header: one byte for ASCII,
    /// up to four otherwise. What follows the last character is never read.
    fn char_header_at(&mut self, lead_at: &str, whole_ascii: Option<&str>) -> String {
        let lead = self.temp();
        self.line(&format!("{lead} = load i8, ptr {lead_at}"));
        // 1 byte below 0xC0, 2 below 0xE0, 3 below 0xF0, else 4.
        let (two, three, four, size_four, size_three, size) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{two} = icmp ult i8 {lead}, -64"));
        self.line(&format!("{three} = icmp ult i8 {lead}, -32"));
        self.line(&format!("{four} = icmp ult i8 {lead}, -16"));
        self.line(&format!("{size_four} = select i1 {four}, i64 3, i64 4"));
        self.line(&format!("{size_three} = select i1 {three}, i64 2, i64 {size_four}"));
        self.line(&format!("{size} = select i1 {two}, i64 1, i64 {size_three}"));
        // An ASCII character is its byte, with byte 23 saying one byte.
        let (plain, wide) = (self.temp(), self.temp());
        self.line(&format!("{plain} = icmp sge i8 {lead}, 0"));
        self.line(&format!("{wide} = zext i8 {lead} to i64"));
        let ascii_header = self.header_of([&wide, "0", "-9151314442816847872"]);
        let (other, done) = (self.label("char_other"), self.label("char_done"));
        let mut ascii_ends = vec![self.current.clone()];
        if let Some(whole) = whole_ascii {
            let each = self.label("char_lead");
            self.line(&format!("br i1 {whole}, label %{done}, label %{each}"));
            self.start_block(&each);
            ascii_ends.push(each);
        }
        self.line(&format!("br i1 {plain}, label %{done}, label %{other}"));
        self.start_block(&other);
        let out = self.place_for_text();
        self.line(&format!("store {HEADER} zeroinitializer, ptr {out}"));
        self.small_copy(&out, lead_at, &size);
        let (tag, byte_tag, tag_at) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{tag} = or i64 {size}, 128"));
        self.line(&format!("{byte_tag} = trunc i64 {tag} to i8"));
        self.line(&format!("{tag_at} = getelementptr inbounds i8, ptr {out}, i64 23"));
        self.line(&format!("store i8 {byte_tag}, ptr {tag_at}"));
        let wider = self.temp();
        self.line(&format!("{wider} = load {HEADER}, ptr {out}"));
        let other_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let header = self.temp();
        let from_ascii: Vec<String> =
            ascii_ends.iter().map(|end| format!("[ {ascii_header}, %{end} ]")).collect();
        self.line(&format!("{header} = phi {HEADER} {}, [ {wider}, %{other_end} ]", from_ascii.join(", ")));
        header
    }

    /// Whether an inline header's text is all ASCII, so its length counts its characters.
    fn inline_ascii(&mut self, header: &str) -> String {
        let [a, b, c] = [0, 1, 2].map(|at| self.header_word(header, at));
        let (low, both, all, high, plain) = (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{low} = and i64 {c}, 72057594037927935"));
        self.line(&format!("{both} = or i64 {a}, {b}"));
        self.line(&format!("{all} = or i64 {both}, {low}"));
        self.line(&format!("{high} = and i64 {all}, -9187201950435737472"));
        self.line(&format!("{plain} = icmp eq i64 {high}, 0"));
        plain
    }

    /// How many characters an inline header's text has: its bytes but the ones that go on a
    /// character, `10xxxxxx` in UTF-8, counted a word at a time. Its bytes are zero after its
    /// length, and the last word's top byte, its tag, is left out.
    fn inline_chars(&mut self, header: &str, length: &str) -> String {
        self.declare("declare i64 @llvm.ctpop.i64(i64)");
        let mut chars = length.to_owned();
        for at in 0..3 {
            let mut word = self.header_word(header, at);
            if at == 2 {
                let cleared = self.temp();
                self.line(&format!("{cleared} = and i64 {word}, 72057594037927935"));
                word = cleared;
            }
            let (shifted, not_six, high, on, count, left) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{shifted} = shl i64 {word}, 1"));
            self.line(&format!("{not_six} = xor i64 {shifted}, -1"));
            self.line(&format!("{high} = and i64 {word}, {not_six}"));
            self.line(&format!("{on} = and i64 {high}, -9187201950435737472"));
            self.line(&format!("{count} = call i64 @llvm.ctpop.i64(i64 {on})"));
            self.line(&format!("{left} = sub i64 {chars}, {count}"));
            chars = left;
        }
        chars
    }

    /// `block += more`, taking over the block's owner: an inline `more` is copied straight in
    /// when the block is alone and has room, its characters counted where they are, and anything
    /// else goes as [`Self::append_text`] does.
    pub(crate) fn append_header(&mut self, block: &str, more: &str) -> String {
        use fennec_rt::embed::layout::{TEXT_BYTES, TEXT_CHARS, TEXT_ROOM};
        let in_block = self.header_in_block(more);
        let more_length = self.inline_length(more);
        let at = |emit: &mut Self, offset: i32| emit.offset(block, offset.unsigned_abs());
        let alone = self.alone(block);
        let simple = self.temp();
        self.line(&format!("{simple} = xor i1 {in_block}, true"));
        let (length_at, room_at) = (at(self, TEXT_BYTES), at(self, TEXT_ROOM));
        let (length, room, total, fits, ok, quick) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{length} = load i64, ptr {length_at}"));
        self.line(&format!("{room} = load i64, ptr {room_at}"));
        self.line(&format!("{total} = add i64 {length}, {more_length}"));
        self.line(&format!("{fits} = icmp ule i64 {total}, {room}"));
        self.line(&format!("{ok} = and i1 {alone}, {fits}"));
        self.line(&format!("{quick} = and i1 {ok}, {simple}"));
        let (copy, slow, done) =
            (self.label("append_copy"), self.label("append_slow"), self.label("append_done"));
        self.line(&format!("br i1 {quick}, label %{copy}, label %{slow}, !prof !8"));
        self.start_block(&copy);
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let (start, to) = (self.offset(block, crate::emit::TEXT_START), self.temp());
        self.line(&format!("{to} = getelementptr inbounds i8, ptr {start}, i64 {length}"));
        // With a header's worth of room past the end, its three words go in whole. An inline
        // header is zero after its bytes, so with byte 23 cleared the block stays zero after
        // its end, as the runtime's sorting and hashing expect.
        let (past, roomy) = (self.temp(), self.temp());
        self.line(&format!("{past} = add i64 {length}, 24"));
        self.line(&format!("{roomy} = icmp ule i64 {past}, {room}"));
        let (words, bytes, copied) =
            (self.label("append_words"), self.label("append_bytes"), self.label("append_copied"));
        self.line(&format!("br i1 {roomy}, label %{words}, label %{bytes}, !prof !8"));
        self.start_block(&words);
        for at in 0..3 {
            let (mut word, word_at) = (self.header_word(more, at), self.temp());
            if at == 2 {
                let cleared = self.temp();
                self.line(&format!("{cleared} = and i64 {word}, 72057594037927935"));
                word = cleared;
            }
            self.line(&format!("{word_at} = getelementptr inbounds i8, ptr {to}, i64 {}", at * 8));
            self.line(&format!("store i64 {word}, ptr {word_at}, align 1"));
        }
        self.line(&format!("br label %{copied}"));
        self.start_block(&bytes);
        let place = self.place_for_text();
        self.line(&format!("store {HEADER} {more}, ptr {place}"));
        self.small_copy(&to, &place, &more_length);
        self.line(&format!("br label %{copied}"));
        self.start_block(&copied);
        self.line(&format!("store i64 {total}, ptr {length_at}"));
        let chars_at = at(self, TEXT_CHARS);
        let counted = self.inline_chars(more, &more_length);
        let (chars, more_chars) = (self.temp(), self.temp());
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{more_chars} = add i64 {chars}, {counted}"));
        self.line(&format!("store i64 {more_chars}, ptr {chars_at}"));
        let copy_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        let grown = self.on_blocks([more], |emit, [more]| emit.append_text(block, &more));
        let slow_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!("{result} = phi ptr [ {block}, %{copy_end} ], [ {grown}, %{slow_end} ]"));
        result
    }

    /// Whether a header's text is all ASCII, so a character's place is its byte's.
    fn header_ascii(&mut self, header: &str) -> String {
        use fennec_rt::embed::layout::{TEXT_BYTES, TEXT_CHARS};
        let in_block = self.header_in_block(header);
        let inline = self.inline_ascii(header);
        // A block's counts are only read when it is one.
        let (bytes, chars) = (self.block_word(header, TEXT_BYTES), self.block_word(header, TEXT_CHARS));
        let (same, ascii) = (self.temp(), self.temp());
        self.line(&format!("{same} = icmp eq i64 {bytes}, {chars}"));
        self.line(&format!("{ascii} = select i1 {in_block}, i1 {same}, i1 {inline}"));
        ascii
    }

    /// Copies `length` bytes, 24 at most, with a few moves of whole words rather than a call:
    /// the first and last words of the range, overlapping if they must, so nothing outside it
    /// is read or written.
    pub(crate) fn small_copy(&mut self, to: &str, from: &str, length: &str) {
        self.module.add_helper(
            "fen_small_copy",
            "define internal void @fen_small_copy(ptr %to, ptr %from, i64 %n) #0 {\nentry:\n  %words = icmp uge i64 %n, 8\n  br i1 %words, label %wide, label %narrow\nwide:\n  %first = load i64, ptr %from, align 1\n  %back = add i64 %n, -8\n  %last_from = getelementptr inbounds i8, ptr %from, i64 %back\n  %last = load i64, ptr %last_from, align 1\n  %three = icmp ugt i64 %n, 16\n  br i1 %three, label %middle, label %ends\nmiddle:\n  %mid_from = getelementptr inbounds i8, ptr %from, i64 8\n  %mid = load i64, ptr %mid_from, align 1\n  %mid_to = getelementptr inbounds i8, ptr %to, i64 8\n  store i64 %mid, ptr %mid_to, align 1\n  br label %ends\nends:\n  store i64 %first, ptr %to, align 1\n  %last_to = getelementptr inbounds i8, ptr %to, i64 %back\n  store i64 %last, ptr %last_to, align 1\n  ret void\nnarrow:\n  %half_word = icmp uge i64 %n, 4\n  br i1 %half_word, label %halves, label %bytes\nhalves:\n  %h_first = load i32, ptr %from, align 1\n  %h_back = add i64 %n, -4\n  %h_last_from = getelementptr inbounds i8, ptr %from, i64 %h_back\n  %h_last = load i32, ptr %h_last_from, align 1\n  store i32 %h_first, ptr %to, align 1\n  %h_last_to = getelementptr inbounds i8, ptr %to, i64 %h_back\n  store i32 %h_last, ptr %h_last_to, align 1\n  ret void\nbytes:\n  %none = icmp eq i64 %n, 0\n  br i1 %none, label %done, label %some\nsome:\n  %b_first = load i8, ptr %from, align 1\n  %half = lshr i64 %n, 1\n  %b_mid_from = getelementptr inbounds i8, ptr %from, i64 %half\n  %b_mid = load i8, ptr %b_mid_from, align 1\n  %b_back = add i64 %n, -1\n  %b_last_from = getelementptr inbounds i8, ptr %from, i64 %b_back\n  %b_last = load i8, ptr %b_last_from, align 1\n  store i8 %b_first, ptr %to, align 1\n  %b_mid_to = getelementptr inbounds i8, ptr %to, i64 %half\n  store i8 %b_mid, ptr %b_mid_to, align 1\n  %b_last_to = getelementptr inbounds i8, ptr %to, i64 %b_back\n  store i8 %b_last, ptr %b_last_to, align 1\n  br label %done\ndone:\n  ret void\n}\n\n".to_owned(),
        );
        self.line(&format!("call void @fen_small_copy(ptr {to}, ptr {from}, i64 {length})"));
    }

    /// An inline header of `length` bytes copied from `from`.
    fn inline_from(&mut self, from: &str, length: &str) -> String {
        let out = self.place_for_text();
        self.line(&format!("store {HEADER} zeroinitializer, ptr {out}"));
        self.small_copy(&out, from, length);
        let (tag, byte, tag_at, header) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{tag} = or i64 {length}, 128"));
        self.line(&format!("{byte} = trunc i64 {tag} to i8"));
        self.line(&format!("{tag_at} = getelementptr inbounds i8, ptr {out}, i64 23"));
        self.line(&format!("store i8 {byte}, ptr {tag_at}"));
        self.line(&format!("{header} = load {HEADER}, ptr {out}"));
        header
    }

    /// Defines `@fen_char_place(text, at)`: where character `at` of a string's block starts, in
    /// bytes, counting from the end if `at` is negative, or -1 if there's no such character. A
    /// parser reads the character at the block's cursor, the one before it or the one after it,
    /// so those are found where it's called. An index leaves the cursor on the character after
    /// the one it read, which is where reading in order goes next, and the one before is the one
    /// it read, found a byte back when that's ASCII. Anything else is walked to by
    /// `@fen_char_walk`, which is marked unlikely, so a loop reading in order keeps its registers.
    pub(crate) fn char_place_helper(&mut self) {
        use fennec_rt::embed::layout::{TEXT, TEXT_CHARS, TEXT_CURSOR};
        self.char_walk_helper();
        self.declare("declare i8 @llvm.ctlz.i8(i8, i1)");
        self.module.add_helper(
            "fen_char_place",
            format!(
                "define internal i64 @fen_char_place(ptr %text, i64 %at) #2 {{
entry:
  %chars_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_CHARS}
  %chars = load i64, ptr %chars_at
  %inside = icmp ult i64 %at, %chars
  br i1 %inside, label %near, label %walk, !prof !8
near:
  %cursor_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_CURSOR}
  %cursor = load atomic i64, ptr %cursor_at monotonic, align 8
  %kept_char = and i64 %cursor, 4294967295
  %kept_byte = lshr i64 %cursor, 32
  %again = icmp eq i64 %at, %kept_char
  br i1 %again, label %done, label %back_or_not
back_or_not:
  %before = add i64 %kept_char, -1
  %is_back = icmp eq i64 %at, %before
  br i1 %is_back, label %back, label %next_or_not
back:
  %back_byte = add i64 %kept_byte, -1
  %back_body = getelementptr inbounds i8, ptr %text, i64 {TEXT}
  %back_at = getelementptr inbounds i8, ptr %back_body, i64 %back_byte
  %back_lead = load i8, ptr %back_at
  %back_top = and i8 %back_lead, -64
  %back_continues = icmp eq i8 %back_top, -128
  br i1 %back_continues, label %walk, label %done, !prof !0
next_or_not:
  %after = add nuw nsw i64 %kept_char, 1
  %is_next = icmp eq i64 %at, %after
  br i1 %is_next, label %next, label %walk
next:
  %lead_at = getelementptr inbounds i8, ptr %text, i64 {TEXT}
  %lead_byte_at = getelementptr inbounds i8, ptr %lead_at, i64 %kept_byte
  %lead = load i8, ptr %lead_byte_at
  %flipped = xor i8 %lead, -1
  %ones = call i8 @llvm.ctlz.i8(i8 %flipped, i1 false)
  %single = icmp eq i8 %ones, 0
  %wide = zext i8 %ones to i64
  %width = select i1 %single, i64 1, i64 %wide
  %stepped = add nuw nsw i64 %kept_byte, %width
  %fits = icmp ult i64 %stepped, 4294967296
  br i1 %fits, label %step, label %walk, !prof !8
step:
  %high = shl nuw i64 %stepped, 32
  %packed = or i64 %high, %at
  store atomic i64 %packed, ptr %cursor_at monotonic, align 8
  br label %done
walk:
  %walked = call i64 @fen_char_walk(ptr %text, i64 %at)
  br label %done
done:
  %place = phi i64 [ %kept_byte, %near ], [ %back_byte, %back ], [ %stepped, %step ], [ %walked, %walk ]
  ret i64 %place
}}

"
            ),
        );
    }

    /// Defines `@fen_char_read(text, at)`: where character `at` of a string's block starts, as
    /// `@fen_char_place` gives it, for an index, which leaves the block's cursor on the character
    /// after it. Reading in order asks for that one next, found at the cursor, and reading the
    /// same one again finds it a byte back when it's ASCII, which changes nothing.
    pub(crate) fn char_read_helper(&mut self) {
        use fennec_rt::embed::layout::{TEXT, TEXT_CHARS, TEXT_CURSOR};
        self.char_walk_helper();
        self.declare("declare i8 @llvm.ctlz.i8(i8, i1)");
        self.module.add_helper(
            "fen_char_read",
            format!(
                "define internal i64 @fen_char_read(ptr %text, i64 %at) #2 {{
entry:
  %chars_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_CHARS}
  %chars = load i64, ptr %chars_at
  %body = getelementptr inbounds i8, ptr %text, i64 {TEXT}
  %cursor_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_CURSOR}
  %inside = icmp ult i64 %at, %chars
  br i1 %inside, label %near, label %walk, !prof !8
near:
  %cursor = load atomic i64, ptr %cursor_at monotonic, align 8
  %kept_char = and i64 %cursor, 4294967295
  %kept_byte = lshr i64 %cursor, 32
  %again = icmp eq i64 %at, %kept_char
  br i1 %again, label %advance, label %back_or_not
back_or_not:
  %before = add i64 %kept_char, -1
  %is_back = icmp eq i64 %at, %before
  br i1 %is_back, label %back, label %walk
back:
  %back_byte = add i64 %kept_byte, -1
  %back_at = getelementptr inbounds i8, ptr %body, i64 %back_byte
  %back_lead = load i8, ptr %back_at
  %back_top = and i8 %back_lead, -64
  %back_continues = icmp eq i8 %back_top, -128
  br i1 %back_continues, label %walk, label %done, !prof !0
walk:
  %walked = call i64 @fen_char_walk(ptr %text, i64 %at)
  %found = icmp sge i64 %walked, 0
  br i1 %found, label %advance, label %done
advance:
  %place = phi i64 [ %kept_byte, %near ], [ %walked, %walk ]
  %lead_at = getelementptr inbounds i8, ptr %body, i64 %place
  %lead = load i8, ptr %lead_at
  %flipped = xor i8 %lead, -1
  %ones = call i8 @llvm.ctlz.i8(i8 %flipped, i1 false)
  %plain = icmp sge i8 %lead, 0
  %wide = zext i8 %ones to i64
  %width = select i1 %plain, i64 1, i64 %wide
  %end = add nuw nsw i64 %place, %width
  %counted = icmp sge i64 %at, 0
  %small = icmp ult i64 %end, 4294967296
  %keeps = and i1 %counted, %small
  br i1 %keeps, label %keep, label %done, !prof !8
keep:
  %after = add nuw nsw i64 %at, 1
  %high = shl nuw i64 %end, 32
  %packed = or i64 %high, %after
  store atomic i64 %packed, ptr %cursor_at monotonic, align 8
  br label %done
done:
  %result = phi i64 [ %back_byte, %back ], [ %walked, %walk ], [ %place, %advance ], [ %place, %keep ]
  ret i64 %result
}}

"
            ),
        );
    }

    /// Defines `@fen_char_walk(text, at)`, which `@fen_char_place` gives anything but the cursor's
    /// character and the next. It walks from whichever is nearest of the start, the end and the
    /// block's cursor, and leaves the cursor there, so reading a string in order takes a step for
    /// each character.
    fn char_walk_helper(&mut self) {
        use fennec_rt::embed::layout::{TEXT, TEXT_BYTES, TEXT_CHARS, TEXT_CURSOR};
        self.module.add_helper(
            "fen_char_walk",
            format!(
                "define internal i64 @fen_char_walk(ptr %text, i64 %at) #0 {{
entry:
  %bytes_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_BYTES}
  %bytes = load i64, ptr %bytes_at
  %chars_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_CHARS}
  %chars = load i64, ptr %chars_at
  %negative = icmp slt i64 %at, 0
  %from_end = add i64 %at, %chars
  %want = select i1 %negative, i64 %from_end, i64 %at
  %inside = icmp ult i64 %want, %chars
  br i1 %inside, label %find, label %outside
outside:
  ret i64 -1
find:
  %cursor_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_CURSOR}
  %cursor = load atomic i64, ptr %cursor_at monotonic, align 8
  %kept_char = and i64 %cursor, 4294967295
  %kept_byte = lshr i64 %cursor, 32
  %kept_ahead = icmp ugt i64 %kept_char, %want
  %kept_up = sub i64 %kept_char, %want
  %kept_down = sub i64 %want, %kept_char
  %kept_far = select i1 %kept_ahead, i64 %kept_up, i64 %kept_down
  %use_kept = icmp ult i64 %kept_far, %want
  %near_char = select i1 %use_kept, i64 %kept_char, i64 0
  %near_byte = select i1 %use_kept, i64 %kept_byte, i64 0
  %near_far = select i1 %use_kept, i64 %kept_far, i64 %want
  %end_far = sub i64 %chars, %want
  %use_end = icmp ult i64 %end_far, %near_far
  %start_char = select i1 %use_end, i64 %chars, i64 %near_char
  %start_byte = select i1 %use_end, i64 %bytes, i64 %near_byte
  %body = getelementptr inbounds i8, ptr %text, i64 {TEXT}
  br label %forward
forward:
  %c = phi i64 [ %start_char, %find ], [ %c_next, %forward_skipped ]
  %p = phi i64 [ %start_byte, %find ], [ %q, %forward_skipped ]
  %short = icmp ult i64 %c, %want
  br i1 %short, label %forward_step, label %backward
forward_step:
  %p_next = add i64 %p, 1
  br label %forward_skip
forward_skip:
  %q = phi i64 [ %p_next, %forward_step ], [ %q_next, %forward_more ]
  %q_at = getelementptr inbounds i8, ptr %body, i64 %q
  %q_byte = load i8, ptr %q_at
  %q_top = and i8 %q_byte, -64
  %q_continues = icmp eq i8 %q_top, -128
  br i1 %q_continues, label %forward_more, label %forward_skipped
forward_more:
  %q_next = add i64 %q, 1
  br label %forward_skip
forward_skipped:
  %c_next = add i64 %c, 1
  br label %forward
backward:
  %b = phi i64 [ %c, %forward ], [ %b_next, %backward_skipped ]
  %r = phi i64 [ %p, %forward ], [ %s, %backward_skipped ]
  %over = icmp ugt i64 %b, %want
  br i1 %over, label %backward_step, label %found
backward_step:
  %r_back = sub i64 %r, 1
  br label %backward_skip
backward_skip:
  %s = phi i64 [ %r_back, %backward_step ], [ %s_back, %backward_more ]
  %s_at = getelementptr inbounds i8, ptr %body, i64 %s
  %s_byte = load i8, ptr %s_at
  %s_top = and i8 %s_byte, -64
  %s_continues = icmp eq i8 %s_top, -128
  br i1 %s_continues, label %backward_more, label %backward_skipped
backward_more:
  %s_back = sub i64 %s, 1
  br label %backward_skip
backward_skipped:
  %b_next = sub i64 %b, 1
  br label %backward
found:
  %small = icmp ult i64 %bytes, 4294967296
  br i1 %small, label %keep, label %done
keep:
  %high = shl i64 %r, 32
  %packed = or i64 %high, %want
  store atomic i64 %packed, ptr %cursor_at monotonic, align 8
  br label %done
done:
  ret i64 %r
}}

"
            ),
        );
    }

    /// `text[index]` on a header: in ASCII text inside it, the byte's character made inline;
    /// anything else as the runtime does it. A block's counts are read as without headers.
    pub(crate) fn header_index(
        &mut self,
        dest: fennec_ir::LocalId,
        text: &str,
        index: &fennec_ir::Operand,
        span: fennec_syntax::Span,
    ) -> Result<(), String> {
        use fennec_rt::embed::layout::{TEXT_BYTES, TEXT_CHARS};
        let (at, _) = self.operand(index)?;
        let in_block = self.header_in_block(text);
        let (block_path, inline_path, slow, done) = (
            self.label("index_block"),
            self.label("index_inline"),
            self.label("index_runtime"),
            self.label("index_done"),
        );
        self.line(&format!("br i1 {in_block}, label %{block_path}, label %{inline_path}"));
        // In a block: its byte and character counts say whether a character is its byte.
        self.start_block(&block_path);
        let first = self.header_word(text, 0);
        let block = self.temp();
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        let (bytes_at, chars_at) =
            (self.offset(&block, TEXT_BYTES.unsigned_abs()), self.offset(&block, TEXT_CHARS.unsigned_abs()));
        let (bytes, chars, ascii, inside, quick) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{bytes} = load i64, ptr {bytes_at}{}", crate::emit::LENGTH_ACCESS));
        self.line(&format!("{chars} = load i64, ptr {chars_at}{}", crate::emit::LENGTH_ACCESS));
        self.line(&format!("{ascii} = icmp eq i64 {bytes}, {chars}"));
        self.line(&format!("{inside} = icmp ult i64 {at}, {bytes}"));
        self.line(&format!("{quick} = and i1 {ascii}, {inside}"));
        let (block_byte, block_wide) = (self.label("index_block_byte"), self.label("index_block_wide"));
        self.line(&format!("br i1 {quick}, label %{block_byte}, label %{block_wide}, !prof !8"));
        // Text that isn't ASCII, or an index from the end: where the character starts is found
        // from the block's cursor, and the runtime only says what's wrong with one outside.
        self.start_block(&block_wide);
        self.char_read_helper();
        let (place, found) = (self.temp(), self.temp());
        self.line(&format!("{place} = call i64 @fen_char_read(ptr {block}, i64 {at})"));
        self.line(&format!("{found} = icmp sge i64 {place}, 0"));
        let block_char = self.label("index_block_char");
        self.line(&format!("br i1 {found}, label %{block_char}, label %{slow}"));
        self.start_block(&block_char);
        let wide_start = self.offset(&block, crate::emit::TEXT_START);
        let lead_at = self.temp();
        self.line(&format!("{lead_at} = getelementptr inbounds i8, ptr {wide_start}, i64 {place}"));
        // An ASCII character is its byte, as in ASCII text, on a way of its own, so a test of it
        // against a character compares the byte.
        let (lead, plain) = (self.temp(), self.temp());
        self.line(&format!("{lead} = load i8, ptr {lead_at}"));
        self.line(&format!("{plain} = icmp sge i8 {lead}, 0"));
        let (wide_ascii, wide_other) = (self.label("index_wide_ascii"), self.label("index_wide_other"));
        self.line(&format!("br i1 {plain}, label %{wide_ascii}, label %{wide_other}"));
        self.start_block(&wide_ascii);
        let lead_wide = self.temp();
        self.line(&format!("{lead_wide} = zext i8 {lead} to i64"));
        let from_ascii = self.header_of([&lead_wide, "0", "-9151314442816847872"]);
        let ascii_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&wide_other);
        let from_wide = self.char_header_at(&lead_at, None);
        let wide_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&block_byte);
        let start = self.offset(&block, crate::emit::TEXT_START);
        let (byte_at, byte, wide) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{byte_at} = getelementptr inbounds i8, ptr {start}, i64 {at}"));
        self.line(&format!("{byte} = load i8, ptr {byte_at}"));
        self.line(&format!("{wide} = zext i8 {byte} to i64"));
        let from_block = self.header_of([&wide, "0", "-9151314442816847872"]);
        let block_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        // Inline: its bytes are in the words.
        self.start_block(&inline_path);
        let (length, plain) = (self.inline_length(text), self.inline_ascii(text));
        let (inside_inline, quick_inline) = (self.temp(), self.temp());
        self.line(&format!("{inside_inline} = icmp ult i64 {at}, {length}"));
        self.line(&format!("{quick_inline} = and i1 {plain}, {inside_inline}"));
        let inline_byte = self.label("index_inline_byte");
        self.line(&format!("br i1 {quick_inline}, label %{inline_byte}, label %{slow}, !prof !8"));
        self.start_block(&inline_byte);
        let place = self.place_for_text();
        self.line(&format!("store {HEADER} {text}, ptr {place}"));
        let (inline_at, inline_byte_value, inline_wide) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{inline_at} = getelementptr inbounds i8, ptr {place}, i64 {at}"));
        self.line(&format!("{inline_byte_value} = load i8, ptr {inline_at}"));
        self.line(&format!("{inline_wide} = zext i8 {inline_byte_value} to i64"));
        let from_inline = self.header_of([&inline_wide, "0", "-9151314442816847872"]);
        let inline_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        self.on_blocks([text], |emit, [block]| {
            emit.declare(&format!(
                "declare void @fennec_str_index_text(ptr sret({HEADER}) align 8, ptr, i64, i64, i64) nounwind"
            ));
            let (place, from, to) = (emit.place_for_text(), i64::from(span.start), i64::from(span.end));
            emit.line(&format!(
                "call void @fennec_str_index_text(ptr sret({HEADER}) align 8 {place}, ptr {block}, i64 {at}, i64 {from}, i64 {to})"
            ));
            emit.leave_if_failed();
            let one = emit.temp();
            emit.line(&format!("{one} = load {HEADER}, ptr {place}"));
            emit.assign(dest, &one)
        })?;
        let after = self.label("index_after");
        self.line(&format!("br label %{after}"));
        self.start_block(&done);
        let one = self.temp();
        self.line(&format!(
            "{one} = phi {HEADER} [ {from_block}, %{block_end} ], [ {from_inline}, %{inline_end} ], [ {from_ascii}, %{ascii_end} ], [ {from_wide}, %{wide_end} ]"
        ));
        self.assign(dest, &one)?;
        self.line(&format!("br label %{after}"));
        self.start_block(&after);
        Ok(())
    }

    /// `text_byte`, `text_size` and `text_part`: a text read by where its bytes are, with no
    /// characters to count. A short part is made inline from its bytes, and the rest by the
    /// runtime, which also says what's wrong with places outside the text.
    pub(crate) fn text_by_place(
        &mut self,
        dest: Option<fennec_ir::LocalId>,
        name: &str,
        args: &[fennec_ir::Operand],
        span: fennec_syntax::Span,
    ) -> Result<(), String> {
        let dest = dest.ok_or("no result")?;
        if !self.headers() {
            return Err(format!("it calls `{name}` where strings aren't headers"));
        }
        let (text, _) = self.operand(args.first().ok_or("no argument")?)?;
        if name == "intrinsics.text_of_code" {
            self.declare(&format!(
                "declare void @fennec_code_header(ptr sret({HEADER}) align 8, i64) nounwind"
            ));
            let (place, found) = (self.place_for_text(), self.temp());
            self.line(&format!(
                "call void @fennec_code_header(ptr sret({HEADER}) align 8 {place}, i64 {text})"
            ));
            self.line(&format!("{found} = load {HEADER}, ptr {place}"));
            return self.assign(dest, &found);
        }
        let length = self.header_byte_length(&text);
        if name == "intrinsics.text_size" {
            return self.assign(dest, &length);
        }
        let (first, _) = self.operand(args.get(1).ok_or("no argument")?)?;
        if name == "intrinsics.text_byte" {
            let found = self.header_byte(&text, &length, &first);
            return self.assign(dest, &found);
        }
        let bytes = self.header_bytes(&text);
        let (last, _) = self.operand(args.get(2).ok_or("no argument")?)?;
        let (ordered, within, size, short, ok, quick) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{ordered} = icmp ule i64 {first}, {last}"));
        self.line(&format!("{within} = icmp ule i64 {last}, {length}"));
        self.line(&format!("{size} = sub i64 {last}, {first}"));
        self.line(&format!("{short} = icmp ule i64 {size}, 23"));
        self.line(&format!("{ok} = and i1 {ordered}, {within}"));
        self.line(&format!("{quick} = and i1 {ok}, {short}"));
        let (fast, slow, done) =
            (self.label("part_inline"), self.label("part_runtime"), self.label("part_done"));
        self.line(&format!("br i1 {quick}, label %{fast}, label %{slow}"));
        self.start_block(&fast);
        let at = self.temp();
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {bytes}, i64 {first}"));
        let piece = self.inline_from(&at, &size);
        let fast_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        self.declare("declare ptr @fennec_str_part(ptr, i64, i64, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let made = self.on_blocks([&text], |emit, [block]| {
            let made = emit.temp();
            emit.line(&format!(
                "{made} = call ptr @fennec_str_part(ptr {block}, i64 {first}, i64 {last}, i64 {from}, i64 {to})"
            ));
            made
        });
        self.leave_if_failed();
        let other = self.text_from_block(&made);
        let slow_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!("{result} = phi {HEADER} [ {piece}, %{fast_end} ], [ {other}, %{slow_end} ]"));
        self.assign(dest, &result)
    }

    /// `text_byte(text, at)`: the byte at `at`, or -1 past the end. An inline text's byte is
    /// shifted out of the header's words, so nothing is stored to read it; a block's is loaded.
    fn header_byte(&mut self, text: &str, length: &str, at: &str) -> String {
        let inside = self.temp();
        self.line(&format!("{inside} = icmp ult i64 {at}, {length}"));
        let (read, in_block, inline, joined) = (
            self.label("byte_read"),
            self.label("byte_in_block"),
            self.label("byte_inline"),
            self.label("byte_done"),
        );
        let before = self.current.clone();
        self.line(&format!("br i1 {inside}, label %{read}, label %{joined}, !prof !8"));
        self.start_block(&read);
        let held = self.header_in_block(text);
        self.line(&format!("br i1 {held}, label %{in_block}, label %{inline}"));
        self.start_block(&in_block);
        let first = self.header_word(text, 0);
        let (block, place, byte, from_block) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        self.line(&format!(
            "{place} = getelementptr inbounds i8, ptr {block}, i64 {}",
            crate::emit::TEXT_START
        ));
        let at_byte = self.temp();
        self.line(&format!("{at_byte} = getelementptr inbounds i8, ptr {place}, i64 {at}"));
        self.line(&format!("{byte} = load i8, ptr {at_byte}"));
        self.line(&format!("{from_block} = zext i8 {byte} to i64"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&inline);
        let words = [0, 1, 2].map(|at| self.header_word(text, at));
        let (index, is_first, is_second, later, word) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{index} = lshr i64 {at}, 3"));
        self.line(&format!("{is_first} = icmp eq i64 {index}, 0"));
        self.line(&format!("{is_second} = icmp eq i64 {index}, 1"));
        self.line(&format!("{later} = select i1 {is_second}, i64 {}, i64 {}", words[1], words[2]));
        self.line(&format!("{word} = select i1 {is_first}, i64 {}, i64 {later}", words[0]));
        let (low, shift, moved, from_inline) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{low} = and i64 {at}, 7"));
        self.line(&format!("{shift} = shl i64 {low}, 3"));
        self.line(&format!("{moved} = lshr i64 {word}, {shift}"));
        self.line(&format!("{from_inline} = and i64 {moved}, 255"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let found = self.temp();
        self.line(&format!(
            "{found} = phi i64 [ {from_block}, %{in_block} ], [ {from_inline}, %{inline} ], [ -1, %{before} ]"
        ));
        found
    }

    /// `bytes_text(data, start, end)`: the text in those bytes of `data`, which a format has
    /// checked are inside it. A short run of ASCII is made inline straight from the bytes, with
    /// nothing to allocate, and anything else by the runtime, which checks it's UTF-8 and gives
    /// the empty text when it isn't.
    pub(crate) fn bytes_text(
        &mut self,
        dest: fennec_ir::LocalId,
        args: &[fennec_ir::Operand],
    ) -> Result<(), String> {
        if !self.headers() {
            return Err("it calls `bytes_text` where strings aren't headers".to_owned());
        }
        let (data, _) = self.operand(args.first().ok_or("no bytes")?)?;
        let (start, _) = self.operand(args.get(1).ok_or("no start")?)?;
        let (end, _) = self.operand(args.get(2).ok_or("no end")?)?;
        let (size, short) = (self.temp(), self.temp());
        self.line(&format!("{size} = sub i64 {end}, {start}"));
        self.line(&format!("{short} = icmp ule i64 {size}, 23"));
        let base = self.offset(&data, fennec_rt::embed::bytes::DATA_OFFSET);
        let at = self.temp();
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {base}, i64 {start}"));
        let (head, body, tail, fast, slow, done) = (
            self.label("run_ascii"),
            self.label("run_byte"),
            self.label("run_checked"),
            self.label("run_inline"),
            self.label("run_runtime"),
            self.label("run_done"),
        );
        let before = self.current.clone();
        self.line(&format!("br i1 {short}, label %{head}, label %{slow}"));
        // Every byte under 128, or-ed together, is under 128.
        self.start_block(&head);
        let (step, seen, more, next, joined) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{step} = phi i64 [ 0, %{before} ], [ {next}, %{body} ]"));
        self.line(&format!("{seen} = phi i64 [ 0, %{before} ], [ {joined}, %{body} ]"));
        self.line(&format!("{more} = icmp ult i64 {step}, {size}"));
        self.line(&format!("br i1 {more}, label %{body}, label %{tail}"));
        self.start_block(&body);
        let (place, byte, wide) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{place} = getelementptr inbounds i8, ptr {at}, i64 {step}"));
        self.line(&format!("{byte} = load i8, ptr {place}"));
        self.line(&format!("{wide} = zext i8 {byte} to i64"));
        self.line(&format!("{joined} = or i64 {seen}, {wide}"));
        self.line(&format!("{next} = add i64 {step}, 1"));
        self.line(&format!("br label %{head}"));
        self.start_block(&tail);
        let ascii = self.temp();
        self.line(&format!("{ascii} = icmp ult i64 {seen}, 128"));
        self.line(&format!("br i1 {ascii}, label %{fast}, label %{slow}"));
        self.start_block(&fast);
        let piece = self.inline_from(&at, &size);
        let fast_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        self.declare("declare ptr @fennec_bytes_text_part(ptr, i64, i64) nounwind");
        let made = self.temp();
        self.line(&format!("{made} = call ptr @fennec_bytes_text_part(ptr {data}, i64 {start}, i64 {end})"));
        let other = self.text_from_block(&made);
        let slow_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!("{result} = phi {HEADER} [ {piece}, %{fast_end} ], [ {other}, %{slow_end} ]"));
        self.assign(dest, &result)
    }

    /// `text[start:end]` on a header (an absent bound as `i64::MIN`): a short piece of ASCII text
    /// with both bounds inside it made inline from its bytes, and anything else by the runtime.
    pub(crate) fn header_slice(
        &mut self,
        text: &str,
        (start, end): (&str, &str),
        span: fennec_syntax::Span,
    ) -> String {
        let ascii = self.header_ascii(text);
        let length = self.header_byte_length(text);
        let lowest = i64::MIN;
        let (no_start, no_end, from, to) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{no_start} = icmp eq i64 {start}, {lowest}"));
        self.line(&format!("{no_end} = icmp eq i64 {end}, {lowest}"));
        self.line(&format!("{from} = select i1 {no_start}, i64 0, i64 {start}"));
        self.line(&format!("{to} = select i1 {no_end}, i64 {length}, i64 {end}"));
        let (low_ok, ordered, high_ok, size, short) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{low_ok} = icmp sge i64 {from}, 0"));
        self.line(&format!("{ordered} = icmp sle i64 {from}, {to}"));
        self.line(&format!("{high_ok} = icmp sle i64 {to}, {length}"));
        self.line(&format!("{size} = sub i64 {to}, {from}"));
        self.line(&format!("{short} = icmp ule i64 {size}, 23"));
        let mut quick = ascii;
        for test in [low_ok, ordered, high_ok, short] {
            let both = self.temp();
            self.line(&format!("{both} = and i1 {quick}, {test}"));
            quick = both;
        }
        let (fast, other, slow, done) = (
            self.label("slice_inline"),
            self.label("slice_other"),
            self.label("slice_runtime"),
            self.label("slice_done"),
        );
        self.line(&format!("br i1 {quick}, label %{fast}, label %{other}, !prof !8"));
        self.start_block(&fast);
        let bytes = self.header_bytes(text);
        let piece_at = self.temp();
        self.line(&format!("{piece_at} = getelementptr inbounds i8, ptr {bytes}, i64 {from}"));
        let piece = self.inline_from(&piece_at, &size);
        let fast_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&other);
        let (wide_piece, wide_end) = self.wide_slice(text, (start, end), (&no_end, &slow, &done));
        self.start_block(&slow);
        self.declare("declare ptr @fennec_str_slice(ptr, i64, i64, i64, i64) nounwind");
        let (span_from, span_to) = (i64::from(span.start), i64::from(span.end));
        let made = self.on_blocks([text], |emit, [block]| {
            let made = emit.temp();
            emit.line(&format!(
                "{made} = call ptr @fennec_str_slice(ptr {block}, i64 {start}, i64 {end}, i64 {span_from}, i64 {span_to})"
            ));
            made
        });
        self.leave_if_failed();
        let other = self.text_from_block(&made);
        let slow_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!(
            "{result} = phi {HEADER} [ {piece}, %{fast_end} ], [ {other}, %{slow_end} ], [ {wide_piece}, %{wide_end} ]"
        ));
        result
    }

    /// The part of a block's text that isn't ASCII from character `start` to `end`, both inside
    /// it and in order, when it's short enough to be inline: where each starts is found from the
    /// block's cursor, as for an index. Anything else goes to `slow`. Gives back the part and the
    /// block it's made in, which goes on to `done`.
    fn wide_slice(
        &mut self,
        text: &str,
        (start, end): (&str, &str),
        (no_end, slow, done): (&str, &str, &str),
    ) -> (String, String) {
        use fennec_rt::embed::layout::{TEXT_BYTES, TEXT_CHARS, TEXT_CURSOR};
        let in_block = self.header_in_block(text);
        let wide = self.label("slice_wide");
        self.line(&format!("br i1 {in_block}, label %{wide}, label %{slow}"));
        self.start_block(&wide);
        let first = self.header_word(text, 0);
        let block = self.temp();
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        let (bytes, chars) = (self.temp(), self.temp());
        let (bytes_at, chars_at) =
            (self.offset(&block, TEXT_BYTES.unsigned_abs()), self.offset(&block, TEXT_CHARS.unsigned_abs()));
        self.line(&format!("{bytes} = load i64, ptr {bytes_at}"));
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        let lowest = i64::MIN;
        let (no_start, from, to) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{no_start} = icmp eq i64 {start}, {lowest}"));
        self.line(&format!("{from} = select i1 {no_start}, i64 0, i64 {start}"));
        self.line(&format!("{to} = select i1 {no_end}, i64 {chars}, i64 {end}"));
        let (low_ok, ordered, high_ok, inside) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{low_ok} = icmp sge i64 {from}, 0"));
        self.line(&format!("{ordered} = icmp sle i64 {from}, {to}"));
        self.line(&format!("{high_ok} = icmp sle i64 {to}, {chars}"));
        let both = self.temp();
        self.line(&format!("{both} = and i1 {low_ok}, {ordered}"));
        self.line(&format!("{inside} = and i1 {both}, {high_ok}"));
        let find = self.label("slice_find");
        self.line(&format!("br i1 {inside}, label %{find}, label %{slow}"));
        self.start_block(&find);
        self.char_place_helper();
        // The end of the text has no character to find: it's where the bytes end.
        let place = |emit: &mut Self, at: &str| {
            let (found, at_end, place) = (emit.temp(), emit.temp(), emit.temp());
            emit.line(&format!("{found} = call i64 @fen_char_place(ptr {block}, i64 {at})"));
            emit.line(&format!("{at_end} = icmp eq i64 {at}, {chars}"));
            emit.line(&format!("{place} = select i1 {at_end}, i64 {bytes}, i64 {found}"));
            place
        };
        // The end first, which a parser has just read up to and the cursor is at.
        let to_place = place(self, &to);
        // A short part that's ASCII is as many bytes back from the end as it has characters: it's
        // made from those bytes, and kept when they're all ASCII, as they then are its characters.
        let body = self.offset(&block, crate::emit::TEXT_START);
        let (count, guess, short_count, in_text, try_guess) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{count} = sub i64 {to}, {from}"));
        self.line(&format!("{guess} = sub i64 {to_place}, {count}"));
        self.line(&format!("{short_count} = icmp ule i64 {count}, 23"));
        self.line(&format!("{in_text} = icmp sge i64 {guess}, 0"));
        self.line(&format!("{try_guess} = and i1 {short_count}, {in_text}"));
        let (guessing, walking, joined) =
            (self.label("slice_guess"), self.label("slice_walk"), self.label("slice_joined"));
        self.line(&format!("br i1 {try_guess}, label %{guessing}, label %{walking}"));
        self.start_block(&guessing);
        let guess_at = self.temp();
        self.line(&format!("{guess_at} = getelementptr inbounds i8, ptr {body}, i64 {guess}"));
        let guessed = self.inline_from(&guess_at, &count);
        let plain = self.inline_ascii(&guessed);
        let guessed_end = self.current.clone();
        self.line(&format!("br i1 {plain}, label %{joined}, label %{walking}, !prof !8"));
        // Otherwise the start is walked back to, with the cursor put back at the end, where
        // reading carries on.
        self.start_block(&walking);
        let (cursor_at, cursor) = (self.offset(&block, TEXT_CURSOR.unsigned_abs()), self.temp());
        self.line(&format!("{cursor} = load atomic i64, ptr {cursor_at} monotonic, align 8"));
        let from_place = place(self, &from);
        self.line(&format!("store atomic i64 {cursor}, ptr {cursor_at} monotonic, align 8"));
        let (size, short) = (self.temp(), self.temp());
        self.line(&format!("{size} = sub i64 {to_place}, {from_place}"));
        self.line(&format!("{short} = icmp ule i64 {size}, 23"));
        let inline = self.label("slice_wide_inline");
        self.line(&format!("br i1 {short}, label %{inline}, label %{slow}"));
        self.start_block(&inline);
        let piece_at = self.temp();
        self.line(&format!("{piece_at} = getelementptr inbounds i8, ptr {body}, i64 {from_place}"));
        let walked = self.inline_from(&piece_at, &size);
        let walked_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let piece = self.temp();
        self.line(&format!(
            "{piece} = phi {HEADER} [ {guessed}, %{guessed_end} ], [ {walked}, %{walked_end} ]"
        ));
        let end_block = self.current.clone();
        self.line(&format!("br label %{done}"));
        (piece, end_block)
    }

    /// A header's length in characters: a block's count, or an inline string's bytes that
    /// don't continue a character, worked out only for an inline one.
    pub(crate) fn header_chars(&mut self, header: &str) -> String {
        use fennec_rt::embed::layout::TEXT_CHARS;
        let in_block = self.header_in_block(header);
        let (block_path, inline_path, done) =
            (self.label("chars_block"), self.label("chars_inline"), self.label("chars_done"));
        self.line(&format!("br i1 {in_block}, label %{block_path}, label %{inline_path}"));
        self.start_block(&block_path);
        let first = self.header_word(header, 0);
        let (block, chars_at, counted) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        self.line(&format!(
            "{chars_at} = getelementptr inbounds i8, ptr {block}, i64 {}",
            TEXT_CHARS.unsigned_abs()
        ));
        self.line(&format!("{counted} = load i64, ptr {chars_at}"));
        let block_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&inline_path);
        // A byte continues a character when it's 10xxxxxx: count the bytes that aren't.
        self.declare("declare i64 @llvm.ctpop.i64(i64)");
        let length = self.inline_length(header);
        let mut continuing = "0".to_owned();
        for at in 0..3 {
            let word = self.header_word(header, at);
            let word = if at == 2 {
                let low = self.temp();
                self.line(&format!("{low} = and i64 {word}, 72057594037927935"));
                low
            } else {
                word
            };
            let (shifted, not_next, both, ones, count, sum) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{shifted} = shl i64 {word}, 1"));
            self.line(&format!("{not_next} = xor i64 {shifted}, -1"));
            self.line(&format!("{both} = and i64 {word}, {not_next}"));
            self.line(&format!("{ones} = and i64 {both}, -9187201950435737472"));
            self.line(&format!("{count} = call i64 @llvm.ctpop.i64(i64 {ones})"));
            self.line(&format!("{sum} = add i64 {continuing}, {count}"));
            continuing = sum;
        }
        let inline = self.temp();
        self.line(&format!("{inline} = sub i64 {length}, {continuing}"));
        let inline_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let chars = self.temp();
        self.line(&format!("{chars} = phi i64 [ {counted}, %{block_end} ], [ {inline}, %{inline_end} ]"));
        chars
    }

    /// `text.code()` on a header: one ASCII byte inline is its own code, and anything else
    /// goes to the runtime, which fails if it isn't one character.
    pub(crate) fn header_code(
        &mut self,
        dest: fennec_ir::LocalId,
        text: &str,
        span: fennec_syntax::Span,
    ) -> Result<(), String> {
        let (first, last) = (self.header_word(text, 0), self.header_word(text, 2));
        let (one_byte, ascii, quick) = (self.temp(), self.temp(), self.temp());
        // Inline, one byte long, and that byte below 0x80.
        self.line(&format!("{one_byte} = icmp eq i64 {last}, -9151314442816847872"));
        self.line(&format!("{ascii} = icmp ult i64 {first}, 128"));
        self.line(&format!("{quick} = and i1 {one_byte}, {ascii}"));
        let (fast, slow, done) =
            (self.label("code_byte"), self.label("code_runtime"), self.label("code_done"));
        self.line(&format!("br i1 {quick}, label %{fast}, label %{slow}, !prof !8"));
        self.start_block(&fast);
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        self.declare("declare i64 @fennec_str_code(ptr, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let counted = self.on_blocks([text], |emit, [block]| {
            let counted = emit.temp();
            emit.line(&format!("{counted} = call i64 @fennec_str_code(ptr {block}, i64 {from}, i64 {to})"));
            counted
        });
        self.leave_if_failed();
        let slow_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let code = self.temp();
        self.line(&format!("{code} = phi i64 [ {first}, %{fast} ], [ {counted}, %{slow_end} ]"));
        self.assign(dest, &code)
    }

    /// `text.isdigit()` on a header, as std's says it: one or more of the digits 0 to 9. Looking
    /// at bytes gives the same answer as looking at characters, as every byte of a character
    /// that isn't ASCII is above `9`. Of one character it comes down to a byte's range.
    pub(crate) fn header_isdigit(
        &mut self,
        dest: fennec_ir::LocalId,
        text: &fennec_ir::Operand,
    ) -> Result<(), String> {
        self.module.add_helper(
            "fen_digits",
            "define internal i1 @fen_digits(ptr %bytes, i64 %length) #0 {\nentry:\n  %any = icmp ne i64 %length, 0\n  br i1 %any, label %next, label %no\nnext:\n  %at = phi i64 [ 0, %entry ], [ %after, %digit ]\n  %place = getelementptr inbounds i8, ptr %bytes, i64 %at\n  %byte = load i8, ptr %place\n  %off = add i8 %byte, -48\n  %is = icmp ult i8 %off, 10\n  br i1 %is, label %digit, label %no\ndigit:\n  %after = add i64 %at, 1\n  %more = icmp ult i64 %after, %length\n  br i1 %more, label %next, label %yes\nyes:\n  ret i1 true\nno:\n  ret i1 false\n}\n\n".to_owned(),
        );
        let (text, _) = self.operand(text)?;
        let in_block = self.header_in_block(&text);
        let (block_path, inline_path, join) =
            (self.label("digits_block"), self.label("digits_inline"), self.label("digits_join"));
        self.line(&format!("br i1 {in_block}, label %{block_path}, label %{inline_path}"));
        self.start_block(&block_path);
        let (first, block_length) = (self.header_word(&text, 0), self.header_word(&text, 1));
        let block = self.temp();
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        let start = self.offset(&block, crate::emit::TEXT_START);
        self.line(&format!("br label %{join}"));
        self.start_block(&inline_path);
        let place = self.place_for_text();
        self.line(&format!("store {HEADER} {text}, ptr {place}"));
        let inline_length = self.inline_length(&text);
        self.line(&format!("br label %{join}"));
        self.start_block(&join);
        let (bytes, length, digits) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{bytes} = phi ptr [ {start}, %{block_path} ], [ {place}, %{inline_path} ]"));
        self.line(&format!(
            "{length} = phi i64 [ {block_length}, %{block_path} ], [ {inline_length}, %{inline_path} ]"
        ));
        self.line(&format!("{digits} = call i1 @fen_digits(ptr {bytes}, i64 {length})"));
        self.assign(dest, &digits)
    }

    /// A word of a header's block at `offset`, read only when it's in a block, and 0 otherwise.
    fn block_word(&mut self, header: &str, offset: i32) -> String {
        let in_block = self.header_in_block(header);
        let before = self.current.clone();
        let (read, done) = (self.label("block_word"), self.label("block_word_done"));
        self.line(&format!("br i1 {in_block}, label %{read}, label %{done}"));
        self.start_block(&read);
        let first = self.header_word(header, 0);
        let (block, at, word) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{block} = inttoptr i64 {first} to ptr"));
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {block}, i64 {}", offset.unsigned_abs()));
        self.line(&format!("{word} = load i64, ptr {at}{}", crate::emit::LENGTH_ACCESS));
        let read_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!("{result} = phi i64 [ {word}, %{read_end} ], [ 0, %{before} ]"));
        result
    }

    /// A piece put on the end of the block pieces are joined into: a header, with headers, and
    /// otherwise a string's block.
    pub(crate) fn append_piece(&mut self, block: &str, piece: &str) -> String {
        if self.headers() { self.append_header(block, piece) } else { self.append_text(block, piece) }
    }

    /// What kind of value an operand is, without making it.
    pub(crate) fn operand_kind(&self, operand: &fennec_ir::Operand) -> Result<Kind, String> {
        use fennec_ir::Operand;
        Ok(match operand {
            Operand::Local(local) => self.kind_of(*local)?.clone(),
            Operand::Int(_) => Kind::Int,
            Operand::Float(_) => Kind::Float,
            Operand::Bool(_) => Kind::Bool,
            Operand::Str(_) => Kind::Str,
            Operand::Bytes(_) => Kind::Bytes,
            Operand::None => Kind::Unit,
        })
    }

    /// `strip` (0), `lstrip` (1) or `rstrip` (2) of a header: an inline ASCII string is cut
    /// here, from its bytes, and anything else as the runtime does it.
    pub(crate) fn header_strip(&mut self, text: &str, which: i64) -> String {
        let in_block = self.header_in_block(text);
        let ascii = self.inline_ascii(text);
        let (inline, quick) = (self.temp(), self.temp());
        self.line(&format!("{inline} = xor i1 {in_block}, true"));
        self.line(&format!("{quick} = and i1 {inline}, {ascii}"));
        let (fast, slow, done) =
            (self.label("strip_inline"), self.label("strip_block"), self.label("strip_done"));
        self.line(&format!("br i1 {quick}, label %{fast}, label %{slow}, !prof !8"));
        self.start_block(&fast);
        // A space, a tab, a line feed, a vertical tab, a form feed or a carriage return.
        let space = |byte: &str, name: &str| {
            format!(
                "  %{name}_off = sub i8 {byte}, 9\n  %{name}_ctl = icmp ult i8 %{name}_off, 5\n  %{name}_sp = icmp eq i8 {byte}, 32\n  %{name} = or i1 %{name}_ctl, %{name}_sp\n"
            )
        };
        let (front, back) = (space("%fb", "fw"), space("%bb", "bw"));
        self.module.add_helper(
            "fen_strip_range",
            format!(
                "define internal void @fen_strip_range(ptr %body, i64 %n, i64 %which, ptr %out) #0 {{\nentry:\n  %keep_front = icmp eq i64 %which, 2\n  br i1 %keep_front, label %ends, label %front_loop\nfront_loop:\n  %s = phi i64 [ 0, %entry ], [ %s_next, %front_on ]\n  %s_in = icmp ult i64 %s, %n\n  br i1 %s_in, label %front_test, label %ends\nfront_test:\n  %fp = getelementptr inbounds i8, ptr %body, i64 %s\n  %fb = load i8, ptr %fp\n{front}  br i1 %fw, label %front_on, label %ends\nfront_on:\n  %s_next = add i64 %s, 1\n  br label %front_loop\nends:\n  %start = phi i64 [ 0, %entry ], [ %s, %front_loop ], [ %s, %front_test ]\n  %keep_back = icmp eq i64 %which, 1\n  br i1 %keep_back, label %scanned, label %back_loop\nback_loop:\n  %e = phi i64 [ %n, %ends ], [ %e_less, %back_on ]\n  %e_in = icmp ugt i64 %e, %start\n  br i1 %e_in, label %back_test, label %scanned\nback_test:\n  %e_less = sub i64 %e, 1\n  %bp = getelementptr inbounds i8, ptr %body, i64 %e_less\n  %bb = load i8, ptr %bp\n{back}  br i1 %bw, label %back_on, label %scanned\nback_on:\n  br label %back_loop\nscanned:\n  %end = phi i64 [ %n, %ends ], [ %e, %back_loop ], [ %e, %back_test ]\n  store i64 %start, ptr %out\n  %end_at = getelementptr inbounds i64, ptr %out, i64 1\n  store i64 %end, ptr %end_at\n  ret void\n}}\n\n"
            ),
        );
        let (bytes, length) = (self.header_bytes(text), self.inline_length(text));
        let range = self.label("%strip_range");
        let _ = writeln!(self.allocas, "  {range} = alloca [2 x i64]");
        self.line(&format!(
            "call void @fen_strip_range(ptr {bytes}, i64 {length}, i64 {which}, ptr {range})"
        ));
        let (end_at, start, end, size, from) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{start} = load i64, ptr {range}"));
        self.line(&format!("{end_at} = getelementptr inbounds i64, ptr {range}, i64 1"));
        self.line(&format!("{end} = load i64, ptr {end_at}"));
        self.line(&format!("{size} = sub i64 {end}, {start}"));
        self.line(&format!("{from} = getelementptr inbounds i8, ptr {bytes}, i64 {start}"));
        // With nothing to strip, it's the same header.
        let (whole, part_label, all) = (self.temp(), self.label("strip_part"), self.current.clone());
        self.line(&format!("{whole} = icmp eq i64 {size}, {length}"));
        self.line(&format!("br i1 {whole}, label %{done}, label %{part_label}"));
        self.start_block(&part_label);
        let piece = self.inline_from(&from, &size);
        let fast_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&slow);
        let stripped = self.on_blocks([text], |emit, [block]| emit.strip_text(&block, which));
        let other = self.text_from_block(&stripped);
        let slow_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!(
            "{result} = phi {HEADER} [ {text}, %{all} ], [ {piece}, %{fast_end} ], [ {other}, %{slow_end} ]"
        ));
        result
    }

    /// Which of two headers comes first, as -1, 0 or 1. Two inline ones compare here: their
    /// bytes, zero after the end, as big-endian words, then their lengths. Others go to the
    /// runtime.
    pub(crate) fn header_order(&mut self, a: &str, b: &str) -> String {
        let (a_block, b_block) = (self.header_in_block(a), self.header_in_block(b));
        let either = self.temp();
        self.line(&format!("{either} = or i1 {a_block}, {b_block}"));
        let (words, runtime, done) =
            (self.label("order_words"), self.label("order_runtime"), self.label("order_done"));
        self.line(&format!("br i1 {either}, label %{runtime}, label %{words}"));
        self.start_block(&words);
        self.declare("declare i64 @llvm.bswap.i64(i64)");
        let mut ordering = String::new();
        let (a_length, b_length) = (self.inline_length(a), self.inline_length(b));
        let (shorter, longer, low, by_length) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{shorter} = icmp ult i64 {a_length}, {b_length}"));
        self.line(&format!("{longer} = icmp ugt i64 {a_length}, {b_length}"));
        self.line(&format!("{low} = select i1 {shorter}, i64 -1, i64 0"));
        self.line(&format!("{by_length} = select i1 {longer}, i64 1, i64 {low}"));
        // From the last word back, so each earlier word decides when it differs.
        for at in (0..3).rev() {
            let (x, y) = (self.header_word(a, at), self.header_word(b, at));
            let (x, y) = if at == 2 {
                let (xm, ym) = (self.temp(), self.temp());
                self.line(&format!("{xm} = and i64 {x}, 72057594037927935"));
                self.line(&format!("{ym} = and i64 {y}, 72057594037927935"));
                (xm, ym)
            } else {
                (x, y)
            };
            let (xs, ys, less, more, same, step_low, step) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{xs} = call i64 @llvm.bswap.i64(i64 {x})"));
            self.line(&format!("{ys} = call i64 @llvm.bswap.i64(i64 {y})"));
            self.line(&format!("{less} = icmp ult i64 {xs}, {ys}"));
            self.line(&format!("{more} = icmp ugt i64 {xs}, {ys}"));
            self.line(&format!("{same} = icmp eq i64 {xs}, {ys}"));
            let after = if ordering.is_empty() { by_length.clone() } else { ordering.clone() };
            self.line(&format!("{step_low} = select i1 {less}, i64 -1, i64 1"));
            self.line(&format!("{step} = select i1 {same}, i64 {after}, i64 {step_low}"));
            let _ = more;
            ordering = step;
        }
        let words_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&runtime);
        let counted = self.on_blocks([a, b], |emit, [x, y]| emit.text_order(&x, &y));
        let runtime_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let result = self.temp();
        self.line(&format!("{result} = phi i64 [ {ordering}, %{words_end} ], [ {counted}, %{runtime_end} ]"));
        result
    }
}
