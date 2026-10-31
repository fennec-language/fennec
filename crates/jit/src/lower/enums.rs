//! Plain enums natively: making cases, reading their fields, `==`, and turning them into
//! runtime objects and back. An enum value is held like a plain struct whose fields are
//! machine words: the case's number, then the case's fields (see `layout`).

use cranelift_codegen::ir::{Block, InstBuilder, MemFlagsData, Value, types};
use cranelift_frontend::Switch;
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, Span};

use super::expr::Lowered;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::layout::{Case, Field, WORD};
use crate::types::Ty;

impl<'a> Lowerer<'a, '_> {
    /// An enum's cases, or `None` for a struct.
    pub(super) fn cases(&self, id: u32) -> Result<Option<&'a [Case]>, Diagnostic> {
        Ok(self.layout(id)?.cases.as_deref())
    }

    /// The plain enum and case an expression names, like `.Circle` or `Shape.Circle`.
    pub(super) fn named_case(&self, expr: &Expr) -> Option<(u32, usize)> {
        let (owner, case) = match &expr.kind {
            ExprKind::Variant(case) => (self.layouts.owner(case)?, case),
            ExprKind::Attr { object, name, optional: false } => match &object.kind {
                ExprKind::Name(owner) if self.local(owner).is_none() => (owner.as_str(), name),
                _ => return None,
            },
            _ => return None,
        };
        let id = self.layouts.id(owner)?;
        let at = self.cases(id).ok()??.iter().position(|written| written.name == *case)?;
        Some((id, at))
    }

    /// `.Circle(radius=1.0)`, made natively. `None` when the arguments don't line up with
    /// the case's fields, which the interpreter then reports.
    pub(super) fn make_case(
        &mut self,
        id: u32,
        at: usize,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let case = self.case(id, at, span)?;
        let Some(values) = self.field_values(&case.fields, args, span)? else { return Ok(None) };
        let mut words = vec![self.int(i64::try_from(at).unwrap_or(0))];
        self.field_words(&case.fields, values, &mut words)?;
        if self.layout(id)?.boxed {
            let size = usize::try_from(self.layout(id)?.size / WORD).unwrap_or(0);
            while words.len() < size {
                words.push(self.int(0));
            }
            return Ok(Some((Some(self.new_block(id, &words)?), Ty::Boxed(id))));
        }
        Ok(Some((Some(self.pad(words, id)?), Ty::Struct(id))))
    }

    fn case(&self, id: u32, at: usize, span: Span) -> Result<&'a Case, Diagnostic> {
        self.cases(id)?.and_then(|cases| cases.get(at)).ok_or_else(|| internal("unknown enum case", span))
    }

    /// An enum value from its first words, the rest zero.
    fn pad(&mut self, mut words: Vec<Value>, id: u32) -> Result<Value, Diagnostic> {
        let size = usize::try_from(self.layout(id)?.size / WORD).unwrap_or(0);
        while words.len() < size {
            words.push(self.int(0));
        }
        Ok(self.struct_of_leaves(words))
    }

    /// A value as the words an enum holds it in.
    pub(super) fn push_words(
        &mut self,
        value: Value,
        ty: Ty,
        words: &mut Vec<Value>,
    ) -> Result<(), Diagnostic> {
        match ty {
            Ty::Struct(id) => {
                let leaves = self.leaves(value, id)?;
                for ((_, ty), leaf) in self.leaf_types(id)?.into_iter().zip(leaves) {
                    self.push_words(leaf, ty, words)?;
                }
            }
            Ty::Obj => words.push(self.help(Helper::CellNew, &[value])?),
            Ty::Float => words.push(self.b.ins().bitcast(types::I64, MemFlagsData::new(), value)),
            Ty::Bool => words.push(self.b.ins().uextend(types::I64, value)),
            _ => words.push(value),
        }
        Ok(())
    }

    /// A value from the words an enum holds it in.
    pub(super) fn read_words(&mut self, words: &[Value], ty: Ty) -> Result<Value, Diagnostic> {
        let span = self.function.def.span;
        let first = || words.first().copied().ok_or_else(|| internal("a field past the enum's end", span));
        Ok(match ty {
            Ty::Struct(id) => {
                let mut leaves = Vec::new();
                for ((_, ty), word) in self.leaf_types(id)?.into_iter().zip(words) {
                    leaves.push(self.read_words(std::slice::from_ref(word), ty)?);
                }
                self.struct_of_leaves(leaves)
            }
            Ty::Obj => self.help(Helper::CellRead, &[first()?])?,
            Ty::Float => self.b.ins().bitcast(types::F64, MemFlagsData::new(), first()?),
            Ty::Bool => self.b.ins().ireduce(types::I8, first()?),
            _ => first()?,
        })
    }

    /// One field of a case, from an enum value's words. When the value is another case, it's
    /// whatever its words hold, so code reads it only once the case is known.
    pub(super) fn case_field(&mut self, words: &[Value], field: &Field) -> Result<Lowered, Diagnostic> {
        let size = match field.ty {
            Ty::Struct(id) => self.layout(id)?.size,
            _ => WORD,
        };
        let start = usize::try_from(field.offset / WORD).unwrap_or(0);
        let end = start + usize::try_from(size / WORD).unwrap_or(0);
        let mine = words.get(start..end).unwrap_or_default().to_vec();
        Ok((Some(self.read_words(&mine, field.ty)?), field.ty))
    }

    /// A block for each case, and a switch on the case's number that jumps to them. Any
    /// other number can't happen, and goes to the first.
    pub(super) fn switch_cases(&mut self, tag: Value, count: usize) -> Vec<Block> {
        let blocks: Vec<Block> = (0..count).map(|_| self.b.create_block()).collect();
        let mut switch = Switch::new();
        for (at, block) in blocks.iter().enumerate() {
            switch.set_entry(u128::try_from(at).unwrap_or(0), *block);
        }
        if let Some(first) = blocks.first() {
            switch.emit(&mut self.b, tag, *first);
        }
        blocks
    }

    /// A runtime object with an enum value's case and fields.
    pub(super) fn box_enum(
        &mut self,
        value: Value,
        id: u32,
        cases: &[Case],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let words = self.leaves(value, id)?;
        let tag = words.first().copied().ok_or_else(|| internal("an enum without words", span))?;
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        let name = self.name_id(&self.layout(id)?.name);
        let blocks = self.switch_cases(tag, cases.len());
        for (case, block) in cases.iter().zip(blocks) {
            self.b.switch_to_block(block);
            self.b.seal_block(block);
            for field in &case.fields {
                let (field_value, ty) = self.case_field(&words, field)?;
                let handle = self.handle_of(field_value, ty, span)?;
                self.call_helper(Helper::Input, &[handle])?;
            }
            let case_name = self.name_id(&case.name);
            let count = self.int(i64::try_from(case.fields.len()).unwrap_or(0));
            let [start, end] = self.span_args(span);
            let handle = self.help(Helper::MakeVariant, &[name, case_name, count, start, end])?;
            self.b.ins().jump(done, &[handle.into()]);
        }
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        self.b.block_params(done).first().copied().ok_or_else(|| internal("lost the boxed enum", span))
    }

    /// A runtime object as a native enum value, failing like the interpreter when it isn't
    /// one of this enum's cases.
    pub(super) fn unbox_enum(
        &mut self,
        handle: Value,
        id: u32,
        cases: &[Case],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let size = usize::try_from(self.layout(id)?.size / WORD).unwrap_or(0);
        let name = self.name_id(&self.layout(id)?.name);
        let [start, end] = self.span_args(span);
        let tag = self.help(Helper::CaseOf, &[handle, name, start, end])?;
        let done = self.b.create_block();
        for _ in 0..size {
            self.b.append_block_param(done, types::I64);
        }
        let blocks = self.switch_cases(tag, cases.len());
        for ((at, case), block) in cases.iter().enumerate().zip(blocks) {
            self.b.switch_to_block(block);
            self.b.seal_block(block);
            let mut words = vec![self.int(i64::try_from(at).unwrap_or(0))];
            for field in &case.fields {
                let copy = self.help(Helper::Copy, &[handle])?;
                let field_name = self.name_id(&field.name);
                let optional = self.b.ins().iconst(types::I8, 0);
                let [start, end] = self.span_args(span);
                let value = self.help(Helper::Field, &[copy, field_name, optional, start, end])?;
                let value = self.convert(Some(value), Ty::Obj, field.ty, span)?;
                self.push_words(value, field.ty, &mut words)?;
            }
            while words.len() < size {
                words.push(self.int(0));
            }
            let args: Vec<_> = words.into_iter().map(Into::into).collect();
            self.b.ins().jump(done, &args);
        }
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        let words = self.b.block_params(done).to_vec();
        Ok(self.struct_of_leaves(words))
    }

    /// `a == b` or `a != b` on two plain structs or enums of the same type: whether every
    /// leaf has the same bits, which is how the interpreter compares their fields.
    pub(super) fn struct_equality(
        &mut self,
        op: BinOp,
        (a, a_ty): Lowered,
        (b, b_ty): Lowered,
    ) -> Result<Option<Lowered>, Diagnostic> {
        if !matches!(op, BinOp::Eq | BinOp::NotEq) || a_ty != b_ty {
            return Ok(None);
        }
        if let (Ty::Boxed(id), Some(a), Some(b)) = (a_ty, a, b) {
            let same = self.boxed_equality(op == BinOp::NotEq, a, b, id)?;
            return Ok(Some((Some(same), Ty::Bool)));
        }
        let (Ty::Struct(id), Some(a), Some(b)) = (a_ty, a, b) else { return Ok(None) };
        let left = self.leaves(a, id)?;
        let right = self.leaves(b, id)?;
        let mut same = self.b.ins().iconst(types::I8, 1);
        for ((_, ty), (x, y)) in self.leaf_types(id)?.into_iter().zip(left.into_iter().zip(right)) {
            let (x, y) = if ty == Ty::Float {
                let flags = MemFlagsData::new();
                (self.b.ins().bitcast(types::I64, flags, x), self.b.ins().bitcast(types::I64, flags, y))
            } else {
                (x, y)
            };
            let equal = self.b.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, x, y);
            same = self.b.ins().band(same, equal);
        }
        if op == BinOp::NotEq {
            same = self.b.ins().bxor_imm_u(same, 1);
        }
        Ok(Some((Some(same), Ty::Bool)))
    }
}
