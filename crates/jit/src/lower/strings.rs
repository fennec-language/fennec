//! Strings natively. A string is the address of a block the runtime owns: its owner count,
//! shape, length in bytes and in characters, then its UTF-8 bytes, zero-padded to a whole
//! word and never shorter than one. Native code reads lengths, indexes ASCII strings
//! through a table of one-character strings, and compares against short literals word by
//! word. Making new strings, and anything that walks UTF-8, goes to the runtime.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use fennec_interp::embed::heap;
use fennec_syntax::{BinOp, Diagnostic, Span, Stmt};

use super::expr::Lowered;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::types::Ty;

/// Where a string's length in bytes, its length in characters and its bytes start.
const BYTES: i32 = 16;
const CHARS: i32 = 24;
const TEXT: i32 = 32;

impl Lowerer<'_, '_> {
    /// A string literal: the address of a block that lives for the whole run.
    pub(super) fn literal(&mut self, text: &str) -> Lowered {
        let value = if let (Some(base), Some(slots)) = (self.slots_base, &mut self.shared.slots) {
            let at = i32::try_from(slots.literal(text) * 8).unwrap_or(0);
            self.b.ins().load(types::I64, MemFlagsData::trusted(), base, at)
        } else {
            let address = heap::literal(&mut self.shared.tables, text);
            self.int(address)
        };
        self.literal_of.insert(value, text.to_owned());
        (Some(self.borrow(value)), Ty::Str)
    }

    /// The text of a literal a value is, when it's one.
    fn literal_text(&self, value: Value) -> Option<String> {
        self.literal_of.get(&value).cloned()
    }

    /// `len(text)`: its length in characters.
    pub(super) fn string_length(&mut self, text: Value) -> Result<Value, Diagnostic> {
        let length = self.b.ins().load(types::I64, MemFlagsData::trusted(), text, CHARS);
        self.consume(Some(text), Ty::Str)?;
        Ok(length)
    }

    /// The one-character string for an ASCII byte, from the shared table. It lives for the
    /// whole run.
    fn ascii(&mut self, byte: Value) -> Value {
        let table = if let Some(base) = self.slots_base {
            self.b.ins().load(types::I64, MemFlagsData::trusted(), base, 8)
        } else {
            let table = heap::ascii_table(&mut self.shared.tables);
            self.int(table)
        };
        let offset = self.b.ins().imul_imm_s(byte, 8);
        let entry = self.b.ins().iadd(table, offset);
        self.b.ins().load(types::I64, MemFlagsData::trusted(), entry, 0)
    }

    /// `for name in text`, walking its bytes. An ASCII character comes from the shared
    /// table; the runtime makes any other. The loop keeps an owner of the string, so
    /// changing the name it came from inside the loop doesn't change what the loop sees.
    pub(super) fn for_text(
        &mut self,
        name: &str,
        text: Value,
        span: Span,
        body: &[Stmt],
    ) -> Result<(), Diagnostic> {
        let text = self.own(text, Ty::Str);
        self.drop_temps()?;
        self.open_scope();
        self.bind_native(&format!("\0for{}", span.start), Ty::Str, Some(text), false);
        let length = self.b.ins().load(types::I64, MemFlagsData::trusted(), text, BYTES);
        let [at, step] = [(); 2].map(|()| self.b.declare_var(types::I64));
        let zero = self.int(0);
        self.b.def_var(at, zero);
        self.b.def_var(step, zero);
        let [header, ascii, wide, inside, advance, exit] = [(); 6].map(|()| self.b.create_block());
        self.b.set_cold_block(wide);
        self.b.append_block_param(inside, types::I64);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let here = self.b.use_var(at);
        let more = self.b.ins().icmp(IntCC::SignedLessThan, here, length);
        self.b.ins().brif(more, ascii, &[], exit, &[]);
        self.b.switch_to_block(ascii);
        let place = self.b.ins().iadd(text, here);
        let byte = self.b.ins().uload8(types::I64, MemFlagsData::trusted(), place, TEXT);
        let one = self.int(1);
        self.b.def_var(step, one);
        let wider = self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, byte, 0x80);
        let quick = self.b.create_block();
        self.b.ins().brif(wider, wide, &[], quick, &[]);
        self.b.switch_to_block(quick);
        let char = self.ascii(byte);
        self.b.ins().jump(inside, &[char.into()]);
        self.b.switch_to_block(wide);
        // A UTF-8 lead byte says how many bytes its character takes.
        let [two, three, four] = [0xC0, 0xE0, 0xF0]
            .map(|lead| self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, byte, lead));
        let size = [two, three, four].into_iter().fold(one, |size, longer| {
            let longer = self.b.ins().uextend(types::I64, longer);
            self.b.ins().iadd(size, longer)
        });
        self.b.def_var(step, size);
        let char = self.help(Helper::StrCharAt, &[text, here])?;
        self.b.ins().jump(inside, &[char.into()]);
        self.b.switch_to_block(inside);
        let char =
            self.b.block_params(inside).first().copied().ok_or_else(|| internal("lost a character", span))?;
        let depth = self.scopes.len();
        self.open_scope();
        self.bind_native(name, Ty::Str, Some(char), false);
        self.loops.push(super::Loop { next: advance, done: exit, depth, put_back: None });
        let lowered = self.tail(body);
        self.loops.pop();
        lowered?;
        self.close_scope()?;
        if !self.is_dead() {
            self.b.ins().jump(advance, &[]);
        }
        self.b.switch_to_block(advance);
        let (here, size) = (self.b.use_var(at), self.b.use_var(step));
        let after = self.b.ins().iadd(here, size);
        self.b.def_var(at, after);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(exit);
        self.close_scope()
    }

    /// `text[at]`. An ASCII string's character comes from a table; any other goes to the
    /// runtime, which also fails like the interpreter for a position past either end.
    pub(super) fn string_index(&mut self, text: Value, at: Value, span: Span) -> Result<Lowered, Diagnostic> {
        let chars = self.b.ins().load(types::I64, MemFlagsData::trusted(), text, CHARS);
        let bytes = self.b.ins().load(types::I64, MemFlagsData::trusted(), text, BYTES);
        let negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, at, 0);
        let from_end = self.b.ins().iadd(at, chars);
        let resolved = self.b.ins().select(negative, from_end, at);
        let ascii = self.b.ins().icmp(IntCC::Equal, chars, bytes);
        let inside = self.b.ins().icmp(IntCC::UnsignedLessThan, resolved, chars);
        let fast = self.b.ins().band(ascii, inside);
        let (quick, slow, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(slow);
        self.b.append_block_param(done, types::I64);
        self.b.ins().brif(fast, quick, &[], slow, &[]);
        self.b.switch_to_block(quick);
        self.b.seal_block(quick);
        let place = self.b.ins().iadd(text, resolved);
        let byte = self.b.ins().uload8(types::I64, MemFlagsData::trusted(), place, TEXT);
        let one = self.ascii(byte);
        self.b.ins().jump(done, &[one.into()]);
        self.b.switch_to_block(slow);
        self.b.seal_block(slow);
        let [start, end] = self.span_args(span);
        let one = self.help(Helper::StrIndex, &[text, at, start, end])?;
        self.b.ins().jump(done, &[one.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        let one =
            self.b.block_params(done).first().copied().ok_or_else(|| internal("lost a string", span))?;
        self.consume(Some(text), Ty::Str)?;
        Ok((Some(one), Ty::Str))
    }

    /// `text[from:to]`, made by the runtime.
    pub(super) fn string_slice(
        &mut self,
        text: Value,
        bounds: [Option<Value>; 2],
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let [from, to] = bounds.map(|bound| bound.unwrap_or_else(|| self.int(i64::MIN)));
        let [start, end] = self.span_args(span);
        let part = self.help(Helper::StrSlice, &[text, from, to, start, end])?;
        self.consume(Some(text), Ty::Str)?;
        Ok((Some(part), Ty::Str))
    }

    /// Whether a string equals a literal. Short literals are compared a word at a time.
    pub(super) fn equals_literal(&mut self, text: Value, literal: &str) -> Result<Value, Diagnostic> {
        let bytes = literal.as_bytes();
        if bytes.len() > 8 {
            let address =
                self.literal(literal).0.ok_or_else(|| internal("a lost literal", Span::default()))?;
            return self.help(Helper::BoxEqual, &[text, address]);
        }
        let mut word = [0; 8];
        for (to, from) in word.iter_mut().zip(bytes) {
            *to = *from;
        }
        let length = self.b.ins().load(types::I64, MemFlagsData::trusted(), text, BYTES);
        let first = self.b.ins().load(types::I64, MemFlagsData::trusted(), text, TEXT);
        let same_length =
            self.b.ins().icmp_imm_s(IntCC::Equal, length, i64::try_from(bytes.len()).unwrap_or(0));
        let same_text = self.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from_le_bytes(word));
        Ok(self.b.ins().band(same_length, same_text))
    }

    /// `text < "c"` and the like for a one-byte literal: comparing the first byte, and the
    /// length when it ties.
    fn order_literal(&mut self, op: BinOp, text: Value, byte: u8) -> Value {
        let length = self.b.ins().load(types::I64, MemFlagsData::trusted(), text, BYTES);
        let first = self.b.ins().uload8(types::I64, MemFlagsData::trusted(), text, TEXT);
        let empty = self.b.ins().icmp_imm_s(IntCC::Equal, length, 0);
        let below = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThan, first, i64::from(byte));
        let less = self.b.ins().bor(empty, below);
        let tied = self.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(byte));
        let single = self.b.ins().icmp_imm_s(IntCC::Equal, length, 1);
        let exact = self.b.ins().band(tied, single);
        let at_most = self.b.ins().bor(less, exact);
        match op {
            BinOp::Lt => less,
            BinOp::LtEq => at_most,
            BinOp::Gt => self.b.ins().bxor_imm_u(at_most, 1),
            _ => self.b.ins().bxor_imm_u(less, 1),
        }
    }

    /// An operator on two strings natively, or `None` when it isn't one strings have.
    pub(super) fn string_binary(
        &mut self,
        op: BinOp,
        (a, a_ty): Lowered,
        (b, b_ty): Lowered,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let (Ty::Str, Ty::Str, Some(a), Some(b)) = (a_ty, b_ty, a, b) else { return Ok(None) };
        let result = match op {
            BinOp::Add => {
                let joined = self.help(Helper::StrConcat, &[a, b])?;
                (Some(joined), Ty::Str)
            }
            BinOp::Eq | BinOp::NotEq => {
                let same = match (self.literal_text(a), self.literal_text(b)) {
                    (_, Some(literal)) => self.equals_literal(a, &literal)?,
                    (Some(literal), None) => self.equals_literal(b, &literal)?,
                    (None, None) => self.help(Helper::BoxEqual, &[a, b])?,
                };
                let same = if op == BinOp::NotEq { self.b.ins().bxor_imm_u(same, 1) } else { same };
                (Some(same), Ty::Bool)
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => (Some(self.order(op, a, b)?), Ty::Bool),
            BinOp::In | BinOp::NotIn => {
                let found = self.help(Helper::StrContains, &[b, a])?;
                let found = if op == BinOp::NotIn { self.b.ins().bxor_imm_u(found, 1) } else { found };
                (Some(found), Ty::Bool)
            }
            _ => return Ok(None),
        };
        self.consume(Some(a), Ty::Str)?;
        self.consume(Some(b), Ty::Str)?;
        Ok(Some(result))
    }

    /// `a < b` and the like on strings.
    fn order(&mut self, op: BinOp, a: Value, b: Value) -> Result<Value, Diagnostic> {
        let one_byte = |text: Option<String>| {
            text.and_then(|text| match text.as_bytes() {
                [byte] if byte.is_ascii() => Some(*byte),
                _ => None,
            })
        };
        if let Some(byte) = one_byte(self.literal_text(b)) {
            return Ok(self.order_literal(op, a, byte));
        }
        if let Some(byte) = one_byte(self.literal_text(a)) {
            let flipped = match op {
                BinOp::Lt => BinOp::Gt,
                BinOp::LtEq => BinOp::GtEq,
                BinOp::Gt => BinOp::Lt,
                _ => BinOp::LtEq,
            };
            return Ok(self.order_literal(flipped, b, byte));
        }
        let ordering = self.help(Helper::StrCompare, &[a, b])?;
        let cc = match op {
            BinOp::Lt => IntCC::SignedLessThan,
            BinOp::LtEq => IntCC::SignedLessThanOrEqual,
            BinOp::Gt => IntCC::SignedGreaterThan,
            _ => IntCC::SignedGreaterThanOrEqual,
        };
        Ok(self.b.ins().icmp_imm_s(cc, ordering, 0))
    }
}
