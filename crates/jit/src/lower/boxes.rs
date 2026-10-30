//! Boxed values natively: structs and enums that contain themselves, and the types that
//! hold them. A boxed value is the address of a block the runtime owns: an owner count,
//! the block's shape, then its fields as words, as an enum's are (see `layout`).
//!
//! Native code counts owners itself. A name owns its value, except a name that can't
//! change and whose value something else keeps alive: a parameter, or a field a `match`
//! binds from such a name. Reading a name borrows its value; an expression that makes a
//! value (building one, calling a function) gives back an owned one. Whatever keeps a
//! value (a name, a field, a return) owns it, taking a new owner for a borrowed one, and
//! whatever only uses an owned value releases it after.

use std::rc::Rc;

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use fennec_interp::packed::Element;
use fennec_syntax::{Arg, Diagnostic, Span};

use super::expr::Lowered;
use super::{Lowerer, Scope, internal};
use crate::helpers::Helper;
use crate::layout::{Field, Layouts, WORD};
use crate::types::Ty;

/// Bytes before a block's fields: its owner count and its shape.
const HEADER: i64 = 16;

impl<'a> Lowerer<'a, '_> {
    /// One more owner for a block.
    pub(super) fn retain(&mut self, address: Value) {
        let count = self.b.ins().load(types::I64, MemFlagsData::trusted(), address, 0);
        let more = self.b.ins().iadd_imm_s(count, 1);
        self.b.ins().store(MemFlagsData::trusted(), more, address, 0);
    }

    /// One owner fewer for a block, freeing it when that was the last. A zero address,
    /// a name that was never given a value, holds nothing.
    pub(super) fn release(&mut self, address: Value) -> Result<(), Diagnostic> {
        let (held, last, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(last);
        self.b.ins().brif(address, held, &[], done, &[]);
        self.b.switch_to_block(held);
        self.b.seal_block(held);
        let count = self.b.ins().load(types::I64, MemFlagsData::trusted(), address, 0);
        let fewer = self.b.ins().iadd_imm_s(count, -1);
        self.b.ins().store(MemFlagsData::trusted(), fewer, address, 0);
        self.b.ins().brif(fewer, done, &[], last, &[]);
        self.b.switch_to_block(last);
        self.b.seal_block(last);
        self.call_helper(Helper::Free, &[address])?;
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        Ok(())
    }

    /// A value something will keep: a new owner for a borrowed boxed value, under a value
    /// of its own, so it's known to be owned.
    pub(super) fn own(&mut self, value: Value, ty: Ty) -> Value {
        if matches!(ty, Ty::Boxed(_)) && self.borrowed.contains(&value) {
            self.retain(value);
            return self.b.ins().iadd_imm_s(value, 0);
        }
        value
    }

    /// Done with a value: an owned boxed value is released.
    pub(super) fn consume(&mut self, value: Option<Value>, ty: Ty) -> Result<(), Diagnostic> {
        match (value, ty) {
            (Some(value), Ty::Boxed(_)) if !self.borrowed.contains(&value) => self.release(value),
            _ => Ok(()),
        }
    }

    /// Marks a boxed value as borrowed: something else keeps it alive.
    pub(super) fn borrow(&mut self, value: Value) -> Value {
        self.borrowed.insert(value);
        value
    }

    /// Releases the boxed values a scope's names own.
    pub(super) fn release_scope(&mut self, scope: &Scope) -> Result<(), Diagnostic> {
        let mut owned: Vec<_> = scope
            .locals
            .values()
            .filter(|local| matches!(local.ty, Ty::Boxed(_)))
            .filter_map(|local| local.var)
            .filter(|var| !self.borrowed_names.contains(&var.index()))
            .collect();
        owned.sort_unstable_by_key(|var| var.index());
        for var in owned {
            let value = self.b.use_var(var);
            self.release(value)?;
        }
        Ok(())
    }

    /// Releases what the names of every scope deeper than `depth` own, before leaving them.
    pub(super) fn release_scopes(&mut self, depth: usize) -> Result<(), Diagnostic> {
        let scopes = std::mem::take(&mut self.scopes);
        let released = scopes.iter().skip(depth).rev().try_for_each(|scope| self.release_scope(scope));
        self.scopes = scopes;
        released
    }

    /// Where a block's fields start.
    fn fields_at(&mut self, address: Value) -> Value {
        self.b.ins().iadd_imm_s(address, HEADER)
    }

    /// A block's words: all of its fields, an enum's case first.
    pub(super) fn block_words(&mut self, address: Value, id: u32) -> Result<Vec<Value>, Diagnostic> {
        let size = self.layout(id)?.size / WORD;
        let mut words = Vec::new();
        for word in 0..size {
            let offset = i32::try_from(i64::from(word * WORD) + HEADER).unwrap_or(0);
            words.push(self.b.ins().load(types::I64, MemFlagsData::trusted(), address, offset));
        }
        Ok(words)
    }

    /// A new block holding `words`, its fields: an owned boxed value.
    pub(super) fn new_block(&mut self, id: u32, words: &[Value]) -> Result<Value, Diagnostic> {
        let shape = self.int(i64::from(id));
        let address = self.help(Helper::Alloc, &[shape])?;
        let fields = self.fields_at(address);
        for (at, word) in words.iter().enumerate() {
            let offset = i32::try_from(at * 8).unwrap_or(0);
            self.b.ins().store(MemFlagsData::trusted(), *word, fields, offset);
        }
        Ok(address)
    }

    /// Words for a boxed value's fields from their values, owning the boxed ones.
    pub(super) fn field_words(
        &mut self,
        fields: &[Field],
        values: Vec<Value>,
        words: &mut Vec<Value>,
    ) -> Result<(), Diagnostic> {
        for (field, value) in fields.iter().zip(values) {
            let value = self.own(value, field.ty);
            self.push_words(value, field.ty, words)?;
        }
        Ok(())
    }

    /// `Node(...)` for a boxed struct, made natively. `None` when the arguments don't line
    /// up with its fields.
    pub(super) fn construct_boxed(
        &mut self,
        id: u32,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let layout = self.layout(id)?;
        let Some(values) = self.field_values(&layout.fields, args, span)? else { return Ok(None) };
        let mut words = Vec::new();
        self.field_words(&layout.fields, values, &mut words)?;
        Ok(Some((Some(self.new_block(id, &words)?), Ty::Boxed(id))))
    }

    /// A boxed struct's field. A boxed field is borrowed when the struct is; otherwise it
    /// gets an owner of its own before the struct is let go.
    pub(super) fn boxed_field(
        &mut self,
        (object, id): (Value, u32),
        field: &Field,
    ) -> Result<Lowered, Diagnostic> {
        let fields = self.fields_at(object);
        let size = match field.ty {
            Ty::Struct(inner) => self.layout(inner)?.size,
            _ => WORD,
        };
        let mut words = Vec::new();
        for word in 0..size / WORD {
            let offset = i32::try_from(field.offset + word * WORD).unwrap_or(0);
            words.push(self.b.ins().load(types::I64, MemFlagsData::trusted(), fields, offset));
        }
        let value = self.read_words(&words, field.ty)?;
        let borrowed = self.borrowed.contains(&object);
        if matches!(field.ty, Ty::Boxed(_)) {
            if borrowed {
                self.borrow(value);
            } else {
                self.retain(value);
            }
        }
        self.consume(Some(object), Ty::Boxed(id))?;
        Ok((Some(value), field.ty))
    }

    /// Gives a name's block one owner, the name, copying it first if it's shared, so a
    /// change to it changes nothing else. Gives back where its fields start.
    pub(super) fn unshare(&mut self, var: cranelift_frontend::Variable) -> Result<Value, Diagnostic> {
        let address = self.b.use_var(var);
        let count = self.b.ins().load(types::I64, MemFlagsData::trusted(), address, 0);
        let shared = self.b.ins().icmp_imm_s(IntCC::NotEqual, count, 1);
        let (copy, done) = (self.b.create_block(), self.b.create_block());
        self.b.append_block_param(done, types::I64);
        self.b.set_cold_block(copy);
        self.b.ins().brif(shared, copy, &[], done, &[address.into()]);
        self.b.switch_to_block(copy);
        self.b.seal_block(copy);
        let copied = self.help(Helper::Unshare, &[address])?;
        self.b.ins().jump(done, &[copied.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        let unshared = self.b.block_params(done).first().copied();
        let unshared = unshared.ok_or_else(|| internal("lost a block", self.function.def.span))?;
        self.b.def_var(var, unshared);
        Ok(self.fields_at(unshared))
    }

    /// `value.method(args)` on a boxed value: a direct native call to its type's method,
    /// which borrows `self`.
    pub(super) fn call_boxed_method(
        &mut self,
        object: &fennec_syntax::Expr,
        id: u32,
        method: &str,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let key = format!("{}.{method}", self.layout(id)?.name);
        let Some(compiled) = self.shared.compiled.get(&key) else { return Ok(None) };
        let params = compiled.params.clone();
        if !compiled.takes_self
            || params.len() != args.len() + 1
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
            || params.iter().any(|(_, _, is_mut)| *is_mut)
        {
            return Ok(None);
        }
        let (receiver, ty) = self.expr(object)?;
        let mut values = vec![self.convert(receiver, ty, Ty::Boxed(id), object.span)?];
        for (arg, (_, to, _)) in args.iter().zip(params.iter().skip(1)) {
            let (value, from) = self.expr(&arg.value)?;
            if !matches!(to, Ty::Cap(_)) {
                values.push(self.convert(value, from, *to, arg.value.span)?);
            }
        }
        self.call_key(&key, values, span).map(Some)
    }

    /// The name and field an assignment like `node.value = 1` changes, when the name holds
    /// a boxed struct.
    pub(super) fn boxed_target(
        &self,
        object: &fennec_syntax::Expr,
        name: &str,
    ) -> Result<Option<(cranelift_frontend::Variable, &'a Field)>, Diagnostic> {
        let fennec_syntax::ExprKind::Name(root) = &object.kind else { return Ok(None) };
        let Some(super::Local { ty: Ty::Boxed(id), var: Some(var), .. }) = self.local(root) else {
            return Ok(None);
        };
        if self.layout(id)?.cases.is_some() {
            return Ok(None);
        }
        Ok(self.struct_field(id, name)?.map(|field| (var, field)))
    }

    /// Changes one field of the boxed struct a name holds, copying its block first if it's
    /// shared.
    pub(super) fn write_boxed_field(
        &mut self,
        var: cranelift_frontend::Variable,
        field: &Field,
        (value, ty): Lowered,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let value = self.convert(value, ty, field.ty, span)?;
        let value = self.own(value, field.ty);
        let fields = self.unshare(var)?;
        let offset = i32::try_from(field.offset).unwrap_or(0);
        let old = match field.ty {
            Ty::Boxed(_) => Some(self.b.ins().load(types::I64, MemFlagsData::trusted(), fields, offset)),
            _ => None,
        };
        let mut words = Vec::new();
        self.push_words(value, field.ty, &mut words)?;
        for (at, word) in words.into_iter().enumerate() {
            let offset = offset + i32::try_from(at * 8).unwrap_or(0);
            self.b.ins().store(MemFlagsData::trusted(), word, fields, offset);
        }
        match old {
            Some(old) => self.release(old),
            None => Ok(()),
        }
    }

    /// A runtime object with a boxed value's value. An owned value is let go after.
    pub(super) fn box_to_handle(&mut self, value: Value, id: u32) -> Result<Value, Diagnostic> {
        let handle = self.help(Helper::BoxToValue, &[value])?;
        self.consume(Some(value), Ty::Boxed(id))?;
        Ok(handle)
    }

    /// A runtime object as an owned boxed value, failing like the interpreter when it isn't
    /// the type expected.
    pub(super) fn handle_to_box(&mut self, handle: Value, id: u32, span: Span) -> Result<Value, Diagnostic> {
        let shape = self.int(i64::from(id));
        let [start, end] = self.span_args(span);
        self.help(Helper::BoxFromValue, &[handle, shape, start, end])
    }

    /// `a == b` or `a != b` on two boxed values of the same type.
    pub(super) fn boxed_equality(
        &mut self,
        negate: bool,
        a: Value,
        b: Value,
        id: u32,
    ) -> Result<Value, Diagnostic> {
        let same = self.help(Helper::BoxEqual, &[a, b])?;
        self.consume(Some(a), Ty::Boxed(id))?;
        self.consume(Some(b), Ty::Boxed(id))?;
        Ok(if negate { self.b.ins().bxor_imm_u(same, 1) } else { same })
    }
}

/// What the runtime needs to know about each layout's blocks, by the layout's number.
/// Layouts that aren't boxed get a placeholder.
pub(crate) fn shapes(layouts: &Layouts) -> Vec<Rc<Element>> {
    (0..layouts.len())
        .map(|id| {
            let id = u32::try_from(id).unwrap_or(0);
            let described =
                layouts.get(id).filter(|layout| layout.boxed).and_then(|_| describe(layouts, Ty::Struct(id)));
            Rc::new(described.unwrap_or(Element::Int))
        })
        .collect()
}

/// How the runtime reads values of a kind from words.
pub(crate) fn describe(layouts: &Layouts, ty: Ty) -> Option<Element> {
    let fields = |fields: &[Field]| -> Option<Vec<(Rc<str>, Element)>> {
        fields
            .iter()
            .map(|field| Some((Rc::from(field.name.as_str()), describe(layouts, field.ty)?)))
            .collect()
    };
    Some(match ty {
        Ty::Int => Element::Int,
        Ty::Float => Element::Float,
        Ty::Bool => Element::Bool,
        Ty::Boxed(id) => Element::Boxed(usize::try_from(id).ok()?),
        Ty::Struct(id) => {
            let layout = layouts.get(id)?;
            let name = Rc::from(layout.name.as_str());
            match &layout.cases {
                Some(cases) => {
                    let cases: Option<Vec<_>> = cases
                        .iter()
                        .map(|case| Some((Rc::from(case.name.as_str()), fields(&case.fields)?)))
                        .collect();
                    let words = usize::try_from(layout.size / WORD).ok()?;
                    Element::Enum { name, cases: cases?, words }
                }
                None => Element::Struct { name, fields: fields(&layout.fields)? },
            }
        }
        _ => return None,
    })
}
