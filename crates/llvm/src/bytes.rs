//! `bytes` in native code: a block the runtime owns that never changes once filled in. Its length
//! sits where a list's does, so the same checks read it. Anything that makes a new block calls
//! the runtime.

use std::fmt::Write as _;

use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_rt::embed::bytes::DATA_OFFSET;
use fennec_syntax::{BinOp, Span};

use crate::emit::{Emit, ITEM_ACCESS};

impl Emit<'_, '_, '_> {
    /// A literal's block, made the first time it's read, then kept by a global, which owns it for
    /// the whole run. Each thread makes its own, since blocks belong to the thread making them.
    pub(crate) fn bytes_literal(&mut self, data: &[u8]) -> String {
        let hash = fennec_blake3::hash(data).to_hex();
        let (held, kept) = (format!("bd_{hash}"), format!("bc_{hash}"));
        let mut shown = String::with_capacity(data.len() * 3);
        for byte in data {
            let _ = write!(shown, "\\{byte:02X}");
        }
        let length = data.len();
        self.declare(&format!("@\"{held}\" = linkonce_odr constant [{length} x i8] c\"{shown}\""));
        let local = if self.module.threads { "thread_local(initialexec) " } else { "" };
        self.declare(&format!("@\"{kept}\" = linkonce_odr {local}global ptr null"));
        self.declare("declare ptr @fennec_bytes_constant(i64) nounwind");
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let (found, missing, block, value) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{found} = load ptr, ptr @\"{kept}\""));
        self.line(&format!("{missing} = icmp eq ptr {found}, null"));
        let came_from = self.current.clone();
        let (make, joined) = (self.label("make_bytes"), self.label("have_bytes"));
        self.line(&format!("br i1 {missing}, label %{make}, label %{joined}, !prof !0"));
        self.start_block(&make);
        self.line(&format!("{block} = call ptr @fennec_bytes_constant(i64 {length})"));
        let into = self.offset(&block, DATA_OFFSET);
        self.line(&format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {into}, ptr @\"{held}\", i64 {length}, i1 false)"
        ));
        self.line(&format!("store ptr {block}, ptr @\"{kept}\""));
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        self.line(&format!("{value} = phi ptr [ {found}, %{came_from} ], [ {block}, %{make} ]"));
        value
    }

    /// std's byte builder, which a binary format writes into: `bytes_builder()`, then
    /// `bytes_push`, `bytes_push_word`, `bytes_push_text` and `bytes_push_bytes` at its end, and
    /// `bytes_set` of a byte already written. Each writes in place, into a block only it holds with
    /// room enough, and the runtime makes one first when it isn't. Its room is worked out from its
    /// length, as the runtime's `builder_room` has it.
    pub(crate) fn byte_builder(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
    ) -> Result<(), String> {
        if name == "intrinsics.bytes_builder" {
            self.declare("declare ptr @fennec_bytes_builder() nounwind");
            let made = self.temp();
            self.line(&format!("{made} = call ptr @fennec_bytes_builder()"));
            return self.assign(dest.ok_or("no result")?, &made);
        }
        let Some(Operand::Local(data)) = args.first() else {
            return Err(format!("`{name}` given bytes that aren't a local"));
        };
        let slot = self.slot(*data);
        let given = args.get(1).ok_or("no argument")?;
        match name {
            "intrinsics.bytes_push" => {
                let (byte, _) = self.operand(given)?;
                let (block, length) = self.room_for(&slot, "1");
                let (at, small) = (self.byte_place(&block, &length), self.temp());
                self.line(&format!("{small} = trunc i64 {byte} to i8"));
                self.line(&format!("store i8 {small}, ptr {at}"));
                self.lengthen(&block, &length, "1");
            }
            "intrinsics.bytes_push_word" => {
                let (bits, _) = self.operand(given)?;
                let (block, length) = self.room_for(&slot, "8");
                let at = self.byte_place(&block, &length);
                self.line(&format!("store i64 {bits}, ptr {at}, align 1"));
                self.lengthen(&block, &length, "8");
            }
            "intrinsics.bytes_push_text" | "intrinsics.bytes_push_bytes" => {
                let (more, _) = self.operand(given)?;
                let (from, size) = if name == "intrinsics.bytes_push_text" {
                    (self.header_bytes(&more), self.header_byte_length(&more))
                } else {
                    let (size_at, size) = (self.offset(&more, 16), self.temp());
                    self.line(&format!("{size} = load i64, ptr {size_at}"));
                    (self.offset(&more, DATA_OFFSET), size)
                };
                let (block, length) = self.room_for(&slot, &size);
                let at = self.byte_place(&block, &length);
                self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
                self.line(&format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {at}, ptr {from}, i64 {size}, i1 false)"
                ));
                self.lengthen(&block, &length, &size);
            }
            "intrinsics.bytes_set" => {
                let (place, _) = self.operand(given)?;
                let (byte, _) = self.operand(args.get(2).ok_or("no byte")?)?;
                let (block, _) = self.room_for(&slot, "0");
                let (at, small) = (self.byte_place(&block, &place), self.temp());
                self.line(&format!("{small} = trunc i64 {byte} to i8"));
                self.line(&format!("store i8 {small}, ptr {at}"));
            }
            other => return Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
        Ok(())
    }

    /// The builder in `slot`, made the caller's own with room for `extra` more bytes, and its length.
    fn room_for(&mut self, slot: &str, extra: &str) -> (String, String) {
        let block = self.temp();
        self.line(&format!("{block} = load ptr, ptr {slot}"));
        let (length_at, length, wanted) = (self.offset(&block, 16), self.temp(), self.temp());
        self.line(&format!("{length} = load i64, ptr {length_at}"));
        self.line(&format!("{wanted} = add i64 {length}, {extra}"));
        let fits = if extra == "1" {
            // One more byte fits unless it's full: its length a power of two, at least 16.
            let (less, shared_bits, uneven, short, fits) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{less} = sub i64 {length}, 1"));
            self.line(&format!("{shared_bits} = and i64 {length}, {less}"));
            self.line(&format!("{uneven} = icmp ne i64 {shared_bits}, 0"));
            self.line(&format!("{short} = icmp ult i64 {length}, 16"));
            self.line(&format!("{fits} = or i1 {uneven}, {short}"));
            fits
        } else {
            // The next power of two from its length, and at least 16.
            self.declare("declare i64 @llvm.ctlz.i64(i64, i1)");
            let (less, zeros, shift, big, small, room) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{less} = sub i64 {length}, 1"));
            self.line(&format!("{zeros} = call i64 @llvm.ctlz.i64(i64 {less}, i1 false)"));
            self.line(&format!("{shift} = sub i64 64, {zeros}"));
            self.line(&format!("{big} = shl i64 1, {shift}"));
            self.line(&format!("{small} = icmp ule i64 {length}, 16"));
            self.line(&format!("{room} = select i1 {small}, i64 16, i64 {big}"));
            let fits = self.temp();
            self.line(&format!("{fits} = icmp ule i64 {wanted}, {room}"));
            fits
        };
        let ready = self.temp();
        let alone = self.alone(&block);
        self.line(&format!("{ready} = and i1 {alone}, {fits}"));
        let (quick, slow, joined) = (self.label("room"), self.label("grow_bytes"), self.label("roomy"));
        self.line(&format!("br i1 {ready}, label %{quick}, label %{slow}, !prof !8"));
        self.start_block(&slow);
        self.declare("declare ptr @fennec_bytes_grow(ptr, i64) nounwind");
        let grown = self.temp();
        self.line(&format!("{grown} = call ptr @fennec_bytes_grow(ptr {block}, i64 {wanted})"));
        self.line(&format!("store ptr {grown}, ptr {slot}"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&quick);
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let held = self.temp();
        self.line(&format!("{held} = phi ptr [ {block}, %{quick} ], [ {grown}, %{slow} ]"));
        (held, length)
    }

    /// Where byte `at` of a builder is.
    fn byte_place(&mut self, block: &str, at: &str) -> String {
        let data = self.offset(block, DATA_OFFSET);
        let place = self.temp();
        self.line(&format!("{place} = getelementptr inbounds i8, ptr {data}, i64 {at}"));
        place
    }

    /// A builder's length, `more` longer.
    fn lengthen(&mut self, block: &str, length: &str, more: &str) {
        let (length_at, longer) = (self.offset(block, 16), self.temp());
        self.line(&format!("{longer} = add i64 {length}, {more}"));
        self.line(&format!("store i64 {longer}, ptr {length_at}"));
    }

    /// `bytes()`.
    pub(crate) fn empty_bytes(&mut self, dest: LocalId) -> Result<(), String> {
        self.declare("declare ptr @fennec_bytes_new(i64) nounwind");
        let block = self.temp();
        self.line(&format!("{block} = call ptr @fennec_bytes_new(i64 0)"));
        self.assign(dest, &block)
    }

    /// `bytes(list)`, which fails on an item that isn't a byte.
    pub(crate) fn bytes_of_list(&mut self, dest: LocalId, list: &Operand, span: Span) -> Result<(), String> {
        let (list, _) = self.operand(list)?;
        self.declare("declare ptr @fennec_bytes_of_list(ptr, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let block = self.temp();
        self.line(&format!("{block} = call ptr @fennec_bytes_of_list(ptr {list}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        self.assign(dest, &block)
    }

    /// `data[at]`: the position is checked as a list's is.
    pub(crate) fn bytes_index(
        &mut self,
        dest: LocalId,
        bytes: &str,
        index: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let (index, _) = self.operand(index)?;
        let position = self.position(bytes, &index, span);
        let start = self.offset(bytes, DATA_OFFSET);
        let (at, byte, wide) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {start}, i64 {position}"));
        self.line(&format!("{byte} = load i8, ptr {at}"));
        self.line(&format!("{wide} = zext i8 {byte} to i64"));
        self.assign(dest, &wide)
    }

    /// `list(data)`: each byte as an int.
    pub(crate) fn list_of_bytes(&mut self, dest: LocalId, bytes: &Operand) -> Result<(), String> {
        let (bytes, _) = self.operand(bytes)?;
        let length = self.list_length(&bytes);
        let list = self.new_list(ListItem::Int, &length);
        let start = self.offset(&bytes, DATA_OFFSET);
        let (at, end_loop) = self.loop_begin(&length);
        let (from, byte, wide) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{from} = getelementptr inbounds i8, ptr {start}, i64 {at}"));
        self.line(&format!("{byte} = load i8, ptr {from}"));
        self.line(&format!("{wide} = zext i8 {byte} to i64"));
        let place = self.item_address(&list, "i64", &at);
        self.line(&format!("store i64 {wide}, ptr {place}{ITEM_ACCESS}"));
        self.loop_end(&end_loop);
        self.assign(dest, &list)
    }

    /// `byte in data`.
    pub(crate) fn bytes_contains(
        &mut self,
        dest: LocalId,
        bytes: &Operand,
        byte: &Operand,
    ) -> Result<(), String> {
        let ((bytes, _), (byte, _)) = (self.operand(bytes)?, self.operand(byte)?);
        self.declare("declare i8 @fennec_bytes_contains(ptr, i64) nounwind");
        let (found, is_in) = (self.temp(), self.temp());
        self.line(&format!("{found} = call i8 @fennec_bytes_contains(ptr {bytes}, i64 {byte})"));
        self.line(&format!("{is_in} = icmp ne i8 {found}, 0"));
        self.assign(dest, &is_in)
    }

    /// `a + b`, and how two compare. `==` and `!=` go by what's in their blocks, as other blocks do.
    pub(crate) fn bytes_binary(&mut self, op: BinOp, (a, b): (&str, &str)) -> Result<String, String> {
        let result = self.temp();
        if op == BinOp::Add {
            self.declare("declare ptr @fennec_bytes_join(ptr, ptr) nounwind");
            self.line(&format!("{result} = call ptr @fennec_bytes_join(ptr {a}, ptr {b})"));
            return Ok(result);
        }
        let test = match op {
            BinOp::Lt => "slt",
            BinOp::LtEq => "sle",
            BinOp::Gt => "sgt",
            BinOp::GtEq => "sge",
            _ => return Err("it uses an operator on bytes that this backend doesn't take yet".to_owned()),
        };
        self.declare("declare i64 @fennec_bytes_order(ptr, ptr) nounwind");
        let order = self.temp();
        self.line(&format!("{order} = call i64 @fennec_bytes_order(ptr {a}, ptr {b})"));
        self.line(&format!("{result} = icmp {test} i64 {order}, 0"));
        Ok(result)
    }

    /// `data.hex()` or `data.hex(sep)`.
    pub(crate) fn bytes_hex(
        &mut self,
        dest: LocalId,
        bytes: &Operand,
        sep: Option<&Operand>,
    ) -> Result<(), String> {
        let (bytes, _) = self.operand(bytes)?;
        self.declare("declare ptr @fennec_bytes_hex(ptr, ptr) nounwind");
        let text = self.temp();
        match sep {
            Some(sep) => {
                let (sep, _) = self.operand(sep)?;
                self.on_blocks([&sep], |emit, [block]| {
                    emit.line(&format!("{text} = call ptr @fennec_bytes_hex(ptr {bytes}, ptr {block})"));
                });
            }
            None => self.line(&format!("{text} = call ptr @fennec_bytes_hex(ptr {bytes}, ptr null)")),
        }
        let text = self.text_value(&text);
        self.assign(dest, &text)
    }

    /// `data.decode()`: `Ok` with the text, or `Err` with an `InvalidValue`.
    pub(crate) fn bytes_decode(&mut self, dest: LocalId, bytes: &str) -> Result<(), String> {
        self.declare("declare ptr @fennec_bytes_decode(ptr) nounwind");
        self.declare("declare ptr @fennec_bytes_decode_problem(ptr) nounwind");
        let made = self.temp();
        self.line(&format!("{made} = call ptr @fennec_bytes_decode(ptr {bytes})"));
        let held = bytes.to_owned();
        self.made_or_invalid(dest, &made, &|emit| {
            let message = emit.temp();
            emit.line(&format!("{message} = call ptr @fennec_bytes_decode_problem(ptr {held})"));
            message
        })
    }

    /// `bytes.fromhex(text)`: `Ok` with the bytes, or `Err` with an `InvalidValue`.
    pub(crate) fn bytes_from_hex(&mut self, dest: LocalId, text: &Operand) -> Result<(), String> {
        let (text, _) = self.operand(text)?;
        self.declare("declare ptr @fennec_bytes_from_hex(ptr) nounwind");
        self.declare("declare ptr @fennec_bytes_from_hex_problem(ptr) nounwind");
        self.on_blocks([&text], |emit, [block]| {
            let made = emit.temp();
            emit.line(&format!("{made} = call ptr @fennec_bytes_from_hex(ptr {block})"));
            let held = block.clone();
            emit.made_or_invalid(dest, &made, &|emit| {
                let message = emit.temp();
                emit.line(&format!("{message} = call ptr @fennec_bytes_from_hex_problem(ptr {held})"));
                message
            })
        })
    }

    /// `text.encode()`.
    pub(crate) fn text_encoded(&mut self, dest: LocalId, text: &Operand) -> Result<(), String> {
        let (text, _) = self.operand(text)?;
        self.declare("declare ptr @fennec_str_encode(ptr) nounwind");
        let made = self.temp();
        self.on_blocks([&text], |emit, [block]| {
            emit.line(&format!("{made} = call ptr @fennec_str_encode(ptr {block})"));
        });
        self.assign(dest, &made)
    }
}
