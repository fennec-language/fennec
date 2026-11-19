//! What a `with` lends stays in its block. The block's end finishes it: a file is closed and put in
//! its place, a transaction commits, a connection goes back to its pool. So a value that can hold
//! it can't be given back or yielded, kept in a place made outside the block, or sent on a
//! channel, and neither can an arrow function that reads it. A second name for it inside the
//! block is fine, and so are reading it and handing it to a function.
//!
//! It's what has something to finish that's held, a struct or an enum or a value holding one. A
//! number, text or bytes a source lends, or a collection of them, is a copy with nothing to
//! finish, and leaves as any value does: `with saved()? as count: return count`. So does a
//! channel's end, a `Channel`, `Sender` or `Inbox`: it's made to be handed to other tasks, as a
//! task's mailbox `with launch(...) as mailbox` is handed to a hub, and one used after its block
//! fails with `Closed`, which the program deals with.
//!
//! A name in the block given a value that can hold it, as `kept = file` or `kept.append(file)`,
//! may hold it from then on, and is held to the same. The program is held to this as it's
//! written: lowering writes each `with` over a source in as the source's own body, and the program
//! checked again (`Checker::again`) has no `with` names left.

use fennec_syntax::{Diagnostic, Expr, ExprKind};

use crate::function::FnCx;
use crate::types::Type;

/// What to do instead, on every refusal here.
const HELP: &str = "use it inside the block, and keep what you read from it or make with it instead: the block's end finishes it";

/// A name a value reads that may hold what a `with` lent.
struct LentRead {
    name: String,
    lent: Type,
    /// The scope of the `with` block that lent it, counting out from the function's own.
    block: usize,
    /// Whether an arrow function reads it, which holds it whatever the arrow's type.
    by_arrow: bool,
}

impl FnCx<'_> {
    /// Notes a name a `with` block declares for what its source lends, of type `lent`, in the
    /// block's own scope, the innermost.
    pub(crate) fn note_with_lent(&mut self, name: &str, lent: &Type) {
        if self.checker.again {
            return;
        }
        let lent = self.checker.settled(lent);
        if !has_own_type(&lent) || self.channel_end(&lent) {
            return;
        }
        let block = self.innermost_scope();
        self.hold_lent(block, name, (lent, block));
    }

    /// `return value` or `yield value`, by `gives` ("gives back" or "yields"): what a `with` lent
    /// can't leave its block that way.
    pub(crate) fn lent_not_given(&mut self, value: &Expr, ty: &Type, gives: &str) {
        if let Some(read) = self.lent_reads(value, ty).into_iter().next() {
            let message = format!("this {gives} `{}`, which its `with` lends only for its block", read.name);
            self.error(Diagnostic::new(message, value.span).with_help(HELP));
        }
    }

    /// `for names in iter`, its names just declared: an item of what holds what a `with` lent may
    /// hold it too, and is held to its block as that is.
    pub(crate) fn lent_loop(&mut self, names: &[String], iter: &Expr) {
        if self.checker.again || !self.any_lent() {
            return;
        }
        let iter_ty = self.recorded(iter.span).unwrap_or(Type::Unknown);
        let at = self.innermost_scope();
        for read in self.lent_reads(iter, &iter_ty) {
            for name in names {
                let item = self.lookup(name).map(|(item, _)| self.checker.settled(&item));
                if item.is_some_and(|item| self.can_hold(&item, &read.lent, &mut Vec::new())) {
                    self.hold_lent(at, name, (read.lent.clone(), read.block));
                }
            }
        }
    }

    /// `target = value`: what a `with` lent can't be kept in a place made outside its block. A name
    /// inside the block given it may hold it from then on.
    pub(crate) fn lent_kept(&mut self, target: &Expr, value: &Expr) {
        if self.checker.again || !self.any_lent() {
            return;
        }
        let ty = self.recorded(value.span).unwrap_or(Type::Unknown);
        self.lent_into(target, value, &ty, "keeps");
    }

    /// A call that puts `values` into `place`, as `outer.append(file)`, `channel.send(file)` or a
    /// `ref` argument given to a function along with them: what a `with` lent can't go into a place
    /// made outside its block.
    pub(crate) fn lent_put(&mut self, place: &Expr, values: &[&Expr]) {
        if self.checker.again || !self.any_lent() {
            return;
        }
        for value in values {
            let ty = self.recorded(value.span).unwrap_or(Type::Unknown);
            if self.lent_into(place, value, &ty, "puts") {
                return;
            }
        }
    }

    /// What `value` holds that a `with` lent, going into `place` as `does` says: refused when the
    /// place is made outside the block, and noted on it when it's made inside. Whether it was
    /// refused.
    fn lent_into(&mut self, place: &Expr, value: &Expr, ty: &Type, does: &str) -> bool {
        let Some(root) = crate::place::root(place).map(str::to_owned) else { return false };
        let reads = self.lent_reads(value, ty);
        let Some(at) = self.scope_of(&root) else { return false };
        let place_ty = self.recorded(place.span).map(|held| self.checker.settled(&held));
        for read in reads {
            // A place that can't hold it, as an `int` given with `ref` beside it, isn't given it.
            if !read.by_arrow
                && place_ty.as_ref().is_some_and(|held| !self.can_hold(held, &read.lent, &mut Vec::new()))
            {
                continue;
            }
            if at < read.block {
                let message = format!(
                    "this {does} `{}` in `{root}`, which is made outside the block of the `with` that lends it",
                    read.name
                );
                self.error(Diagnostic::new(message, value.span).with_help(HELP));
                return true;
            }
            self.hold_lent(at, &root, (read.lent, read.block));
        }
        false
    }

    /// The names `value`, of type `ty`, reads that may hold what a `with` lent, where what they give
    /// it can hold that too (see `lent_parts`).
    fn lent_reads(&mut self, value: &Expr, ty: &Type) -> Vec<LentRead> {
        let mut found = Vec::new();
        if !self.checker.again && self.any_lent() {
            self.lent_parts(value, Some(ty.clone()), &mut found);
        }
        found
    }

    /// Adds to `found` the names `value` reads that may hold what a `with` lent and that it can
    /// hold. A value built of parts, a list, tuple, set or dict, or a struct being made, is judged
    /// part by part by each part's own type, so `Tx(log=tx.log)` holds no `tx`. Anything else
    /// holds whatever it reads, where its type can hold what's lent: a call can give back what
    /// it's given. An arrow function holds whatever it reads, whatever its type.
    fn lent_parts(&mut self, value: &Expr, ty: Option<Type>, found: &mut Vec<LentRead>) {
        match &value.kind {
            ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
                for item in items {
                    self.lent_parts(item, None, found);
                }
                return;
            }
            ExprKind::Dict(pairs) => {
                for (key, item) in pairs {
                    self.lent_parts(key, None, found);
                    self.lent_parts(item, None, found);
                }
                return;
            }
            ExprKind::Call { callee, args } if self.builds_value(callee) => {
                for arg in args {
                    self.lent_parts(&arg.value, None, found);
                }
                return;
            }
            _ => {}
        }
        let ty =
            ty.or_else(|| self.recorded(value.span)).map_or(Type::Unknown, |ty| self.checker.settled(&ty));
        let mut read = Vec::new();
        let mut captured = Vec::new();
        fennec_syntax::read::in_expr(value, &mut |expr| match &expr.kind {
            ExprKind::Name(name) => read.push(name.clone()),
            ExprKind::Lambda { body, .. } => fennec_syntax::read::in_expr(body, &mut |inner| {
                if let ExprKind::Name(name) = &inner.kind {
                    captured.push(name.clone());
                }
            }),
            _ => {}
        });
        let named = read.into_iter().map(|name| (name, false));
        for (name, by_arrow) in named.chain(captured.into_iter().map(|name| (name, true))) {
            let Some(scope) = self.scope_of(&name) else { continue };
            for (lent, block) in self.lent_in(scope, &name) {
                let held = by_arrow || self.can_hold(&ty, &lent, &mut Vec::new());
                if held && !found.iter().any(|had| had.name == name && had.block == block) {
                    found.push(LentRead { name: name.clone(), lent, block, by_arrow });
                }
            }
        }
    }

    /// Whether a call to `callee` makes a value of what it's given: a struct, an error or an enum's
    /// case, written by its name or as `.Case(...)`.
    fn builds_value(&self, callee: &Expr) -> bool {
        match &callee.kind {
            ExprKind::Variant(_) => true,
            ExprKind::Name(name) => self.checker.program.type_info(name).is_some(),
            ExprKind::Attr { object, .. } => {
                matches!(&object.kind, ExprKind::Name(name) if self.checker.program.type_info(name).is_some())
            }
            _ => false,
        }
    }

    /// Whether `ty` is a channel's end from the standard library: a `Channel`, `Sender` or `Inbox`.
    fn channel_end(&self, ty: &Type) -> bool {
        matches!(ty, Type::Generic(name, _) if matches!(name.as_str(), "Channel" | "Sender" | "Inbox")
            && self.checker.program.type_info(name).is_some_and(|info| info.prelude))
    }

    /// Whether a value of type `ty` can hold one of type `lent`: it is one, or one is among its
    /// items, members or fields, however deep. `seen` stops a type that holds itself.
    fn can_hold(&mut self, ty: &Type, lent: &Type, seen: &mut Vec<String>) -> bool {
        if ty == lent {
            return true;
        }
        match ty {
            Type::List(item)
            | Type::Buffer(item)
            | Type::Array(item, _)
            | Type::Set(item)
            | Type::Optional(item)
            | Type::Iterator(item, _)
            | Type::Table(item) => self.can_hold(item, lent, seen),
            Type::Dict(key, value) | Type::Result(key, value) => {
                self.can_hold(key, lent, seen) || self.can_hold(value, lent, seen)
            }
            Type::Tuple(items) | Type::Union(items) => {
                items.iter().any(|item| self.can_hold(item, lent, seen))
            }
            // A generic type is taken to hold what its type's arguments are, as a channel's
            // messages, held where its fields can't show them.
            Type::Generic(_, args) if args.iter().any(|arg| self.can_hold(arg, lent, seen)) => true,
            Type::Named(owner) | Type::Generic(owner, _) => {
                if seen.contains(owner) {
                    return false;
                }
                seen.push(owner.clone());
                let Some(info) = self.checker.program.type_info(owner) else { return false };
                let mut fields: Vec<(Option<String>, String)> =
                    info.def.fields.iter().map(|field| (None, field.name.clone())).collect();
                for case in &info.def.cases {
                    fields.extend(
                        case.fields.iter().map(|field| (Some(case.name.clone()), field.name.clone())),
                    );
                }
                let args = self.checker.program.type_args(ty);
                fields.into_iter().any(|(case, field)| {
                    let held = self.checker.field_type(owner, case.as_deref(), &field).substitute(&args);
                    let held = self.checker.settled(&held);
                    self.can_hold(&held, lent, seen)
                })
            }
            _ => false,
        }
    }
}

/// Whether a type is, or holds, a struct or an enum: something a source can finish at its block's
/// end, as a file or a transaction is. Numbers, text, bytes and collections of them are copies.
fn has_own_type(ty: &Type) -> bool {
    match ty {
        Type::Named(_) | Type::Generic(..) => true,
        Type::List(item)
        | Type::Buffer(item)
        | Type::Array(item, _)
        | Type::Set(item)
        | Type::Optional(item)
        | Type::Iterator(item, _)
        | Type::Table(item) => has_own_type(item),
        Type::Dict(key, value) | Type::Result(key, value) => has_own_type(key) || has_own_type(value),
        Type::Tuple(items) | Type::Union(items) => items.iter().any(has_own_type),
        _ => false,
    }
}
