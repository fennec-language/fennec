//! Making text natively: f-strings and `str(x)`, and the string methods native code calls
//! without the interpreter: `code`, `join`, and those the standard library writes in Fennec.

use cranelift_codegen::ir::{InstBuilder, Value};
use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FStrPart, Span};

use super::expr::Lowered;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::types::{ListItem, Ty};

impl Lowerer<'_, '_> {
    /// `f"..."`, with each part worked out in order and joined on as it's made.
    pub(super) fn fstring(&mut self, parts: &[FStrPart], span: Span) -> Result<Lowered, Diagnostic> {
        let mut made = None;
        for part in parts {
            let piece = match part {
                FStrPart::Text(text) => {
                    self.literal(text).0.ok_or_else(|| internal("a lost string", span))?
                }
                FStrPart::Expr { expr, spec } => {
                    let value = self.expr(expr)?;
                    self.text_of(value, spec.as_deref(), expr.span)?
                }
            };
            made = Some(match made {
                None => self.own(piece, Ty::Str),
                Some(text) => {
                    let joined = self.append_text(text, piece)?;
                    self.consume(Some(piece), Ty::Str)?;
                    joined
                }
            });
        }
        match made {
            Some(made) => Ok((Some(made), Ty::Str)),
            None => Ok(self.literal("")),
        }
    }

    /// A value's text as a string, formatted by a spec when there is one.
    fn text_of(&mut self, (value, ty): Lowered, spec: Option<&str>, span: Span) -> Result<Value, Diagnostic> {
        if let Some(spec) = spec {
            let handle = self.handle_of(value, ty, span)?;
            let spec = self.literal(spec).0.ok_or_else(|| internal("a lost string", span))?;
            let [start, end] = self.span_args(span);
            return self.help(Helper::SpecText, &[handle, spec, start, end]);
        }
        match (value, ty) {
            (Some(text), Ty::Str) => Ok(text),
            (Some(n), Ty::Int) => self.help(Helper::IntText, &[n]),
            (Some(x), Ty::Float) => self.help(Helper::FloatText, &[x]),
            (Some(flag), Ty::Bool) => {
                let yes = self.literal("True").0.ok_or_else(|| internal("a lost string", span))?;
                let no = self.literal("False").0.ok_or_else(|| internal("a lost string", span))?;
                let text = self.b.ins().select(flag, yes, no);
                Ok(self.borrow(text))
            }
            _ => {
                let handle = self.handle_of(value, ty, span)?;
                self.help(Helper::ValueText, &[handle])
            }
        }
    }

    /// `str(x)`, when the call is to the built-in one.
    pub(super) fn str_call(&mut self, callee: &Expr, args: &[Arg]) -> Result<Option<Lowered>, Diagnostic> {
        let builtin = matches!(&callee.kind, ExprKind::Name(name) if name == "str" && self.local(name).is_none())
            && !self.shared.compiled.contains_key("str");
        let ([arg], true) = (args, builtin) else { return Ok(None) };
        if arg.name.is_some() || arg.is_mut {
            return Ok(None);
        }
        let value = self.expr(&arg.value)?;
        let text = self.text_of(value, None, arg.value.span)?;
        Ok(Some((Some(self.own(text, Ty::Str)), Ty::Str)))
    }

    /// A method on a string native code calls itself, or `None` for any other call.
    pub(super) fn string_method(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return Ok(None) };
        if self.checked_expr(object) != Some(Ty::Str)
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
        {
            return Ok(None);
        }
        let key = format!("str.{name}");
        if let Some(compiled) = self.shared.compiled.get(&key) {
            let params = compiled.params.clone();
            if !compiled.takes_self || params.len() != args.len() + 1 {
                return Ok(None);
            }
            let (text, ty) = self.expr(object)?;
            let mut values = vec![self.convert(text, ty, Ty::Str, object.span)?];
            for (arg, (_, to, _)) in args.iter().zip(params.iter().skip(1)) {
                let (value, from) = self.expr(&arg.value)?;
                values.push(self.convert(value, from, *to, arg.value.span)?);
            }
            return self.call_key_changing(&key, values, span).map(|(result, _)| Some(result));
        }
        match (name.as_str(), args) {
            ("code", []) => {
                let (text, ty) = self.lent_receiver(object)?;
                let text = self.convert(text, ty, Ty::Str, object.span)?;
                let [start, end] = self.span_args(span);
                let code = self.help(Helper::StrCode, &[text, start, end])?;
                self.consume(Some(text), Ty::Str)?;
                Ok(Some((Some(code), Ty::Int)))
            }
            ("strip" | "lstrip" | "rstrip", []) => {
                let (text, ty) = self.lent_receiver(object)?;
                let text = self.convert(text, ty, Ty::Str, object.span)?;
                let which = self.int(match name.as_str() {
                    "lstrip" => 1,
                    "rstrip" => 2,
                    _ => 0,
                });
                let stripped = self.help(Helper::StrStrip, &[text, which])?;
                self.consume(Some(text), Ty::Str)?;
                Ok(Some((Some(stripped), Ty::Str)))
            }
            ("join", [items]) if self.checked_expr(&items.value) == Some(Ty::List(ListItem::Str)) => {
                let list_ty = Ty::List(ListItem::Str);
                let (between, ty) = self.expr(object)?;
                let between = self.convert(between, ty, Ty::Str, object.span)?;
                let (list, from) = self.expr(&items.value)?;
                let list = self.convert(list, from, list_ty, items.value.span)?;
                let joined = self.help(Helper::StrJoin, &[between, list])?;
                self.consume(Some(between), Ty::Str)?;
                self.consume(Some(list), list_ty)?;
                Ok(Some((Some(joined), Ty::Str)))
            }
            _ => Ok(None),
        }
    }

    /// `list[from:to]` on a native list, where a missing end is the same as for a string.
    pub(super) fn list_slice(
        &mut self,
        object: &Expr,
        (from, to): (Option<&Expr>, Option<&Expr>),
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let (list, ty) = self.expr(object)?;
        let list = list.ok_or_else(|| internal("a lost list", span))?;
        let mut bounds = [None, None];
        for (bound, written) in bounds.iter_mut().zip([from, to]) {
            if let Some(written) = written {
                let (value, from) = self.expr(written)?;
                *bound = Some(self.convert(value, from, Ty::Int, written.span)?);
            }
        }
        let [from, to] = bounds.map(|bound| bound.unwrap_or_else(|| self.int(i64::MIN)));
        let part = self.help(Helper::ListSlice, &[list, from, to])?;
        self.consume(Some(list), ty)?;
        Ok((Some(part), ty))
    }
}
