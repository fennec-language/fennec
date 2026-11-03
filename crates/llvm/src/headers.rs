//! A string held as its 24-byte header, when the layouts say so (`FENNEC_INLINE_TEXT`): up to
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
                "  %{side}c = load i64, ptr %{side}b\n  %{side}d = add i64 %{side}c, -1\n  store i64 %{side}d, ptr %{side}b\n  %{side}z = icmp eq i64 %{side}d, 0\n  br i1 %{side}z, label %{side}free, label %{side}kept\n{side}free:\n  call void @fennec_free(ptr %{side}b)\n  br label %{side}kept\n{side}kept:\n"
            );
        }
        text += "  ret i64 %o\n}\n\n";
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
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let (from_a, from_b, out) = (self.place_for_text(), self.place_for_text(), self.place_for_text());
        self.line(&format!("store {HEADER} {a}, ptr {from_a}"));
        self.line(&format!("store {HEADER} {b}, ptr {from_b}"));
        self.line(&format!("store {HEADER} zeroinitializer, ptr {out}"));
        self.line(&format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {out}, ptr {from_a}, i64 {a_length}, i1 false)"
        ));
        let after = self.temp();
        self.line(&format!("{after} = getelementptr inbounds i8, ptr {out}, i64 {a_length}"));
        self.line(&format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {after}, ptr {from_b}, i64 {b_length}, i1 false)"
        ));
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
            fennec_interp::embed::layout::TEXT_BYTES
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
        let (byte, _) = self.operand(args.get(1).ok_or("no argument")?)?;
        let start = self.header_bytes(text);
        let (lead_at, lead) = (self.temp(), self.temp());
        self.line(&format!("{lead_at} = getelementptr inbounds i8, ptr {start}, i64 {byte}"));
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
        let ascii_end = self.current.clone();
        let (other, done) = (self.label("char_other"), self.label("char_done"));
        self.line(&format!("br i1 {plain}, label %{done}, label %{other}"));
        self.start_block(&other);
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let out = self.place_for_text();
        self.line(&format!("store {HEADER} zeroinitializer, ptr {out}"));
        self.line(&format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {out}, ptr {lead_at}, i64 {size}, i1 false)"
        ));
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
        self.line(&format!(
            "{header} = phi {HEADER} [ {ascii_header}, %{ascii_end} ], [ {wider}, %{other_end} ]"
        ));
        self.assign(dest, &header)
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

    /// `block += more`, taking over the block's owner: an inline ASCII `more` is copied straight
    /// in when the block is alone and has room, and anything else goes as [`Self::append_text`]
    /// does.
    pub(crate) fn append_header(&mut self, block: &str, more: &str) -> String {
        use fennec_interp::embed::layout::{TEXT_BYTES, TEXT_CHARS, TEXT_ROOM};
        let in_block = self.header_in_block(more);
        let ascii = self.inline_ascii(more);
        let more_length = self.inline_length(more);
        let at = |emit: &mut Self, offset: i32| emit.offset(block, offset.unsigned_abs());
        let (count, alone, inline, simple) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {block}{}", crate::emit::HEADER_ACCESS));
        self.line(&format!("{alone} = icmp eq i64 {count}, 1"));
        self.line(&format!("{inline} = xor i1 {in_block}, true"));
        self.line(&format!("{simple} = and i1 {inline}, {ascii}"));
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
        self.line(&format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {to}, ptr {place}, i64 {more_length}, i1 false)"
        ));
        self.line(&format!("br label %{copied}"));
        self.start_block(&copied);
        self.line(&format!("store i64 {total}, ptr {length_at}"));
        let chars_at = at(self, TEXT_CHARS);
        let (chars, more_chars) = (self.temp(), self.temp());
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{more_chars} = add i64 {chars}, {more_length}"));
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
        use fennec_interp::embed::layout::{TEXT_BYTES, TEXT_CHARS};
        let in_block = self.header_in_block(header);
        let inline = self.inline_ascii(header);
        // A block's counts are only read when it is one.
        let (bytes, chars) = (self.block_word(header, TEXT_BYTES), self.block_word(header, TEXT_CHARS));
        let (same, ascii) = (self.temp(), self.temp());
        self.line(&format!("{same} = icmp eq i64 {bytes}, {chars}"));
        self.line(&format!("{ascii} = select i1 {in_block}, i1 {same}, i1 {inline}"));
        ascii
    }

    /// An inline header of `length` bytes copied from `from`.
    fn inline_from(&mut self, from: &str, length: &str) -> String {
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let out = self.place_for_text();
        self.line(&format!("store {HEADER} zeroinitializer, ptr {out}"));
        self.line(&format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {out}, ptr {from}, i64 {length}, i1 false)"
        ));
        let (tag, byte, tag_at, header) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{tag} = or i64 {length}, 128"));
        self.line(&format!("{byte} = trunc i64 {tag} to i8"));
        self.line(&format!("{tag_at} = getelementptr inbounds i8, ptr {out}, i64 23"));
        self.line(&format!("store i8 {byte}, ptr {tag_at}"));
        self.line(&format!("{header} = load {HEADER}, ptr {out}"));
        header
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
        use fennec_interp::embed::layout::{TEXT_BYTES, TEXT_CHARS};
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
        self.line(&format!("{bytes} = load i64, ptr {bytes_at}"));
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{ascii} = icmp eq i64 {bytes}, {chars}"));
        self.line(&format!("{inside} = icmp ult i64 {at}, {bytes}"));
        self.line(&format!("{quick} = and i1 {ascii}, {inside}"));
        let block_byte = self.label("index_block_byte");
        self.line(&format!("br i1 {quick}, label %{block_byte}, label %{slow}, !prof !8"));
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
        self.on_blocks([text], |emit, [block]| emit.string_index(dest, &block, index, span))?;
        let after = self.label("index_after");
        self.line(&format!("br label %{after}"));
        self.start_block(&done);
        let one = self.temp();
        self.line(&format!(
            "{one} = phi {HEADER} [ {from_block}, %{block_end} ], [ {from_inline}, %{inline_end} ]"
        ));
        self.assign(dest, &one)?;
        self.line(&format!("br label %{after}"));
        self.start_block(&after);
        Ok(())
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
        let (fast, slow, done) =
            (self.label("slice_inline"), self.label("slice_runtime"), self.label("slice_done"));
        self.line(&format!("br i1 {quick}, label %{fast}, label %{slow}, !prof !8"));
        self.start_block(&fast);
        let bytes = self.header_bytes(text);
        let piece_at = self.temp();
        self.line(&format!("{piece_at} = getelementptr inbounds i8, ptr {bytes}, i64 {from}"));
        let piece = self.inline_from(&piece_at, &size);
        let fast_end = self.current.clone();
        self.line(&format!("br label %{done}"));
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
        self.line(&format!("{result} = phi {HEADER} [ {piece}, %{fast_end} ], [ {other}, %{slow_end} ]"));
        result
    }

    /// A header's length in characters: a block's count, or an inline string's bytes that
    /// don't continue a character, worked out only for an inline one.
    pub(crate) fn header_chars(&mut self, header: &str) -> String {
        use fennec_interp::embed::layout::TEXT_CHARS;
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
    /// goes to the runtime, which fails as the interpreter does.
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
        self.line(&format!("{word} = load i64, ptr {at}"));
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
        self.line(&format!("{result} = phi {HEADER} [ {piece}, %{fast_end} ], [ {other}, %{slow_end} ]"));
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
