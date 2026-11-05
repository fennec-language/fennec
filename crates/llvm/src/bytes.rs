//! `bytes` in native code: a block the runtime owns that never changes once filled in. Its length
//! sits where a list's does, so the same checks read it. Anything that makes a new block calls
//! the runtime, and no interpreter value is involved.

use std::fmt::Write as _;

use fennec_interp::embed::bytes::DATA_OFFSET;
use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
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
        self.declare("declare ptr @fennec_bytes_new(i64) nounwind");
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let (found, missing, block, value) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{found} = load ptr, ptr @\"{kept}\""));
        self.line(&format!("{missing} = icmp eq ptr {found}, null"));
        let came_from = self.current.clone();
        let (make, joined) = (self.label("make_bytes"), self.label("have_bytes"));
        self.line(&format!("br i1 {missing}, label %{make}, label %{joined}, !prof !0"));
        self.start_block(&make);
        self.line(&format!("{block} = call ptr @fennec_bytes_new(i64 {length})"));
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
