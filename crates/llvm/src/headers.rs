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
    fn place_for_text(&mut self) -> String {
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
            BinOp::Add => {
                let joined = self.on_blocks([a, b], |emit, [x, y]| emit.string_binary(op, (&x, &y)))?;
                Ok(self.text_from_block(&joined))
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

    /// Finds a string key's entry by its header, with no block made for it: the entry's
    /// address, or null.
    pub(crate) fn find_header(&mut self, dict: &str, key: &str, value_offset: i64) -> String {
        self.declare("declare i64 @fennec_dict_find_header(ptr, i64, i64, i64) nounwind");
        let [a, b, c] = [0, 1, 2].map(|at| self.header_word(key, at));
        let (value_at, found, entry, chosen, address) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!(
            "{value_at} = call i64 @fennec_dict_find_header(ptr {dict}, i64 {a}, i64 {b}, i64 {c})"
        ));
        self.line(&format!("{found} = icmp ne i64 {value_at}, 0"));
        self.line(&format!("{entry} = sub i64 {value_at}, {value_offset}"));
        self.line(&format!("{chosen} = select i1 {found}, i64 {entry}, i64 0"));
        self.line(&format!("{address} = inttoptr i64 {chosen} to ptr"));
        address
    }
}
