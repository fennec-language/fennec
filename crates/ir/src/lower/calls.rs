//! Calls: to the program's functions and methods, to the standard library's written in
//! Fennec, and to what the engines provide. An argument to a `mut` parameter that's a field
//! is read into a temporary and written back after the call.

use fennec_check::Type;
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, Span};

use super::places::{Place, Step};
use super::{Lowered, Lowering};
use crate::{
    BlockId, ChangedPlace, Inst, InstKind, LocalId, Operand, PlaceStep, Program, Terminator, type_in,
};

/// The local holding what `?.` calls a method on, which no name in a program can be.
const OPTIONAL_OBJECT: &str = "?.";

/// A place a call changes, the local holding it, and what's known of how it was read: the
/// locals on the way, if it can be taken out, and the block they're in.
type WriteBack = (Place, LocalId, Option<(Vec<LocalId>, BlockId)>);

/// A `shared` handle whose lock a call takes, and whether it takes it to write.
type Lock = (Operand, bool);

/// The standard library's functions a program's calls reach, which aren't lowered yet.
pub(super) fn external(program: &Program) -> Vec<String> {
    let mut found: Vec<String> = program
        .functions
        .iter()
        .chain(&program.constants)
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.kind {
            InstKind::Call { function, .. } | InstKind::MakeClosure { function, .. } => {
                Some(function.clone())
            }
            _ => None,
        })
        .filter(|name| {
            !program.functions.iter().any(|function| function.name == *name)
                && !program.not_lowered.iter().any(|skipped| skipped.function == *name)
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The name methods of a built-in type are keyed under.
pub(super) fn owner_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::List(_) => Some("list"),
        Type::Array(..) => Some("array"),
        Type::Buffer(_) => Some("Buffer"),
        Type::Set(_) => Some("set"),
        Type::Dict(..) => Some("dict"),
        Type::Tuple(_) => Some("tuple"),
        Type::Bytes => Some("bytes"),
        Type::Table(_) => Some("Table"),
        Type::Id(_) => Some("Id"),
        Type::Str => Some("str"),
        Type::Int => Some("int"),
        Type::Float => Some("float"),
        Type::Sized(num) => Some(num.name()),
        Type::Bool => Some("bool"),
        Type::Optional(_) => Some("Optional"),
        Type::Named(name) => Some(name),
        _ => None,
    }
}

impl Lowering<'_> {
    /// What a builtin is given, in order, with the receiver first and named arguments last. The
    /// call's name gets the names of those, since it says which they are.
    fn builtin_args(
        &mut self,
        (name, object, changes): (&mut String, Option<&Expr>, bool),
        args: &[Arg],
        write_backs: &mut Vec<WriteBack>,
        span: Span,
    ) -> Lowered<Vec<Operand>> {
        let mut reads = super::order::Reads::default();
        // A field changed in place, like `self.out.append(x)`, is read after the arguments when
        // that's the same: reading it then leaves it taken out, changed and put back with nothing
        // between, so it has one owner while it changes. Read first, an argument with a branch
        // in it, like `1 if x else 0`, would leave it with two, and the change would copy it.
        let after = match object {
            Some(object) if changes => self.read_after_arguments(object),
            _ => None,
        };
        let before = (self.current, self.next_inst(), self.changed.len());
        if let Some(object) = object
            && after.is_none()
        {
            if changes {
                let place = self.place(object, (false, false), write_backs, span)?;
                reads.add_place(place);
            } else {
                let value = self.expr(object)?;
                reads.add(self, value);
            }
        }
        // What a method takes where its parameter is a union, like a key of a `dict[int | str, V]`:
        // a member given there is made a value of the union.
        let unions: Vec<Option<Type>> = object
            .and_then(|object| self.object_type(object))
            .and_then(|ty| fennec_check::builtin_method_params(&ty, name.rsplit('.').next().unwrap_or("")))
            .unwrap_or_default()
            .into_iter()
            .map(|param| matches!(param, Type::Union(_)).then_some(param))
            .collect();
        let mut written = Vec::with_capacity(args.len());
        for (position, arg) in args.iter().enumerate() {
            if arg.is_mut {
                return Err(format!("its call to `{name}` has a `mut` argument"));
            }
            let value = match &arg.value.kind {
                ExprKind::Lambda { params, body } if name == "Table.modify" => {
                    self.arrow(&arg.value, params, body, true)?
                }
                // What's printed, or made text, through a type's own `def str`.
                _ if matches!(name.as_str(), "Stdout.print" | "Stdout.write" | "str") => {
                    self.shown(&arg.value)?
                }
                _ => self.expr(&arg.value)?,
            };
            let value = match unions.get(position) {
                Some(Some(union)) if arg.name.is_none() => self.coerce(value, union, arg.value.span)?,
                _ => value,
            };
            reads.add(self, value);
            written.push(arg.name.clone());
        }
        let mut read = reads.finish(self, span).into_iter();
        let mut values: Vec<Operand> = match (object, after) {
            (Some(object), Some(root)) => {
                vec![self.read_place_after(object, root, before, write_backs, span)?]
            }
            _ => object.iter().filter_map(|_| read.next()).collect(),
        };
        let mut named = Vec::new();
        for (argument, value) in written.into_iter().zip(read) {
            match argument {
                Some(argument) => named.push((argument, value)),
                None => values.push(value),
            }
        }
        named.sort_by(|first, second| first.0.cmp(&second.0));
        for (argument, value) in named {
            *name = format!("{name}.{argument}");
            values.push(value);
        }
        Ok(values)
    }

    /// The local a field changed in place is reached from, when reading the field after the
    /// arguments can't be told apart from reading it first: it's fields of a local all the way,
    /// none of them a `shared` value's, so reading it can't fail and nothing else holds it.
    fn read_after_arguments(&self, object: &Expr) -> Option<LocalId> {
        let (root, path) = super::lists::field_path_of(object)?;
        if path.is_empty() {
            return None;
        }
        let local = self.lookup(root)?;
        let mut ty = self.local_type(local);
        for field in path {
            if self.context.is_handle(&ty) {
                return None;
            }
            let Type::Named(owner) = &ty else { return None };
            let fields = self.struct_fields(owner).ok()?;
            ty = fields.into_iter().find(|(name, _)| name == field)?.1;
        }
        (!matches!(ty, Type::Optional(_))).then_some(local)
    }

    /// Reads a field changed in place once the arguments are worked out. If working them out
    /// changed what it's reached from, it's read where it's written instead, before them, as
    /// left to right says.
    fn read_place_after(
        &mut self,
        object: &Expr,
        root: LocalId,
        (block, at, changes): (BlockId, usize, usize),
        write_backs: &mut Vec<WriteBack>,
        span: Span,
    ) -> Lowered<Operand> {
        let root_changed = self.changed.get(changes..).is_some_and(|since| since.contains(&root));
        let first = self.next_inst();
        let place = self.place(object, (false, false), write_backs, span)?;
        if root_changed {
            let current = self.current;
            let moved: Vec<Inst> = self
                .blocks
                .get_mut(current.0)
                .map(|block| block.insts.drain(first..).collect())
                .unwrap_or_default();
            if let Some(earlier) = self.blocks.get_mut(block.0) {
                let at = at.min(earlier.insts.len());
                earlier.insts.splice(at..at, moved);
            }
            // Read apart from the call, it can't be taken out for it.
            if let Some(last) = write_backs.last_mut() {
                last.2 = None;
            }
        }
        Ok(place)
    }

    /// The types a call writes in brackets, like `Part` in `named[Part](text)`.
    fn written_in(&self, callee: &Expr) -> Vec<Type> {
        let ExprKind::Index { index, .. } = &callee.kind else { return Vec::new() };
        match self.type_at(index.span) {
            Some(Type::Tuple(types)) => types,
            _ => Vec::new(),
        }
    }

    /// What a method call is made on, with the types in its union, when it's a union.
    fn union_receiver<'e>(&self, callee: &'e Expr) -> Option<(&'e Expr, Vec<Type>)> {
        let ExprKind::Attr { object, optional: false, .. } = &callee.kind else { return None };
        match self.type_at(object.span) {
            Some(Type::Union(members)) if self.type_named(object).is_none() => Some((object, members)),
            _ => None,
        }
    }

    /// A method every member of a union has, called on whichever member it holds: a test for
    /// each member but the last, then that member's method, with what it is read as that member.
    /// It's read again in each branch, so it has to be a name or a path of fields from one.
    fn union_method(
        &mut self,
        callee: &Expr,
        (object, members): (&Expr, &[Type]),
        args: &[Arg],
        keep: Option<&Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        if fennec_syntax::dotted(object).is_none() {
            return Err("it calls a method on a union that isn't a name or a field".to_owned());
        }
        let union = self.expr(object)?;
        // What's left of a union after an `is` is held as the union it was, so each member is
        // tested by where it is in that one.
        let Some(Type::Union(held)) = type_in(&self.locals, &union) else {
            return Err("it calls a method on a union that isn't held as one".to_owned());
        };
        let dest = keep.map(|ty| self.temporary(ty.clone()));
        let join = self.new_block();
        for (at, ty) in members.iter().enumerate() {
            let next = (at + 1 < members.len()).then(|| self.new_block());
            if let Some(next) = next {
                let member = held.iter().position(|each| each == ty);
                let member =
                    member.ok_or_else(|| format!("`{ty}` isn't one of {}", Type::Union(held.clone())))?;
                let test = self.temporary(Type::Bool);
                self.emit(InstKind::IsMember { dest: test, value: union.clone(), member }, span);
                self.branch_or(Operand::Local(test), next);
            }
            self.retyped.push((object.span, ty.clone()));
            let member_result = self.call(callee, args, keep.cloned(), span);
            self.retyped.pop();
            if let (Some(dest), Some(result)) = (dest, member_result?) {
                self.emit(InstKind::Copy { dest, value: Operand::Local(result) }, span);
            }
            self.finish(Terminator::Jump(join));
            self.current = next.unwrap_or(join);
        }
        Ok(dest)
    }

    /// A call that isn't made by name: through a closure, with `?.`, `pop()` on a list that may be
    /// empty, or a method of whichever member a union holds.
    fn special_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        keep: Option<&Type>,
        span: Span,
    ) -> Option<Lowered<Option<LocalId>>> {
        if let Some(ty) = self.closure_callee(callee) {
            return Some(self.call_closure(callee, &ty, args, keep.cloned(), span));
        }
        if let ExprKind::Attr { object, name, optional: true } = &callee.kind {
            return Some(self.optional_method((object, name), args, keep.cloned(), span));
        }
        if !self.popping
            && let Some(object) = popped_place(callee, args)
            && matches!(self.type_at(object.span), Some(Type::List(_)))
        {
            return Some(self.pop_or_none(callee, object, keep, span));
        }
        let (object, members) = self.union_receiver(callee)?;
        Some(self.union_method(callee, (object, &members), args, keep, span))
    }

    /// Lowers a call. When it's given the type of what the call gives back, that's kept in a
    /// new temporary.
    pub(super) fn call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        keep: Option<Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        if let Some(lowered) = self.special_call(callee, args, keep.as_ref(), span) {
            return lowered;
        }
        if let Some(value) = self.type_called(callee) {
            let dest = keep.map(|ty| self.temporary(ty));
            if let Some(dest) = dest {
                self.emit(InstKind::Copy { dest, value }, span);
            }
            return Ok(dest);
        }
        // `json.loads[T](text)`: the type it's given is the type of what it gives back.
        self.written_types = self.written_in(callee);
        let callee = match &callee.kind {
            ExprKind::Index { object, .. } => object,
            _ => callee,
        };
        let mut write_backs = Vec::new();
        let dest;
        if let Some((mut name, object, changes)) = self.builtin(callee) {
            // `Buffer[T]()` makes one, which has a name of its own: `Buffer.capacity` is a method.
            if name == "Buffer" && object.is_none() {
                "Buffer.new".clone_into(&mut name);
            }
            if name == "list.remove"
                && let Some(Type::List(held)) = object.and_then(|object| self.type_at(object.span))
                && self.context.signatures.own_equality(&held)
            {
                return Err(super::operators::OWN_EQUALITY.to_owned());
            }
            let mut values = self.builtin_args((&mut name, object, changes), args, &mut write_backs, span)?;
            if let Some(size) = self.array_len(&name, &values) {
                dest = keep.map(|ty| self.temporary(ty));
                if let Some(dest) = dest {
                    self.emit(InstKind::Copy { dest, value: Operand::Int(size) }, span);
                }
                return Ok(dest);
            }
            self.arrays_as_lists(&name, &mut values, span)?;
            // A new array is given its size, which a backend keeping it as a list needs.
            if name == "array.new"
                && let Some(Type::Array(_, size)) = &keep
            {
                values.push(Operand::Int(i64::try_from(*size).map_err(|_| "an array too long to count")?));
            }
            self.fit_list_item(&name, &mut values, span)?;
            if name == "list.pop" && values.len() == 1 && !self.popping {
                dest = self.pop_in_block(values, keep.as_ref(), span)?;
            } else {
                // A value nothing keeps from a builtin that changes nothing, like `sorted(scores)`
                // on a line of its own, is still made and let go of, so a backend that writes it
                // somewhere has somewhere to write it. One that changes what it's given, like
                // `items.pop(i)`, is made for that, and lets go of what it gives itself.
                let keep = keep.or_else(|| self.type_at(span).filter(|ty| !changes && gives_a_value(ty)));
                dest = keep.map(|ty| self.temporary(ty));
                self.emit(InstKind::Builtin { dest, name, args: values, changes }, span);
            }
        } else {
            let (function, values, lock) =
                self.direct((callee, args), keep.as_ref(), &mut write_backs, span)?;
            dest = keep.map(|ty| self.temporary(ty));
            // A generic intrinsic's copy, like `intrinsics.id_slot[Unit]`, is still the intrinsic.
            let intrinsic = function.split_once('[').map_or(function.as_str(), |(base, _)| base);
            if self.context.intrinsics.contains(intrinsic) {
                if LOWERED.contains(&intrinsic) {
                    return self.lowered_intrinsic(intrinsic, dest, values, write_backs, span);
                }
                // One given something by `ref`, like `append_text(ref out, text)`, changes it, and
                // it's put back after, as for any call.
                let name = intrinsic.to_owned();
                let changes = !write_backs.is_empty();
                self.emit(InstKind::Builtin { dest, name, args: values, changes }, span);
                self.write_back(write_backs, span)?;
                return Ok(dest);
            }
            if let Some((handle, write)) = &lock {
                self.emit(InstKind::Lock { handle: handle.clone(), write: *write }, span);
            }
            self.emit(InstKind::Call { dest, function, args: values }, span);
            if let Some((handle, _)) = lock {
                self.emit(InstKind::Unlock { handle }, span);
            }
        }
        self.write_back(write_backs, span)?;
        Ok(dest)
    }

    /// The intrinsics the IR lowers itself, rather than leave to the backend as they are.
    fn lowered_intrinsic(
        &mut self,
        intrinsic: &str,
        dest: Option<LocalId>,
        values: Vec<Operand>,
        write_backs: Vec<WriteBack>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        match intrinsic {
            "intrinsics.channel_receive" => {
                let raw = ("intrinsics.channel_take", 2, 0);
                self.taken_or_none(dest, values, write_backs, raw, span)
            }
            "intrinsics.launch_end" => {
                let raw = ("intrinsics.launch_take", 3, 2);
                self.taken_or_none(dest, values, write_backs, raw, span)
            }
            "intrinsics.crash_cleanup" | "intrinsics.crash_cleanup_done" => {
                let [cleanup] = values.as_slice() else {
                    return Err("a crash's cleanup is one closure".to_owned());
                };
                let cleanup = cleanup.clone();
                let kind = if intrinsic == "intrinsics.crash_cleanup" {
                    InstKind::CrashCleanup { cleanup }
                } else {
                    InstKind::CrashCleanupDone { cleanup }
                };
                self.emit(kind, span);
                Ok(None)
            }
            "intrinsics.abandoning" => {
                let dest = dest.ok_or("whether it's let go is kept")?;
                self.abandoning(dest, span)?;
                Ok(Some(dest))
            }
            _ => self.launch_task(dest, &values, span).map(Some),
        }
    }

    /// `channel_receive[T](channel, timeout_ns, ref why)`, the message, or
    /// `launch_end[E](task, stop_after_ns, failed, ref why)`, the task's failure: taken into a `T`
    /// by the backend's `raw` builtin, with `why` (the argument at `why_at`) set to how it went,
    /// then `Some` of it when `why` is `some_when`, else `None`.
    fn taken_or_none(
        &mut self,
        dest: Option<LocalId>,
        values: Vec<Operand>,
        write_backs: Vec<WriteBack>,
        (raw, why_at, some_when): (&str, usize, i64),
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        let why = values.get(why_at).cloned().ok_or_else(|| format!("`{raw}` without its `why`"))?;
        let wanted = dest.map(|dest| self.local_type(dest));
        let Some(Type::Optional(item)) = wanted else {
            return Err(format!("`{raw}` gives back something not kept as an optional"));
        };
        let taken = self.temporary(*item);
        let take = InstKind::Builtin { dest: Some(taken), name: raw.to_owned(), args: values, changes: true };
        self.emit(take, span);
        let got = self.temporary(Type::Bool);
        let rhs = Operand::Int(some_when);
        self.emit(InstKind::Binary { dest: got, op: BinOp::Eq, lhs: why, rhs }, span);
        self.write_back(write_backs, span)?;
        let dest = dest.ok_or("what's taken, kept by no one")?;
        let (some, none, join) = (self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(got), then: some, otherwise: none });
        self.current = some;
        self.emit(InstKind::MakeSome { dest, value: Operand::Local(taken) }, span);
        self.finish(Terminator::Jump(join));
        self.current = none;
        self.emit(InstKind::Copy { dest, value: Operand::None }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(Some(dest))
    }

    /// Writes back what a call changed, saying which of those places can be taken out for it.
    fn write_back(&mut self, write_backs: Vec<WriteBack>, span: Span) -> Lowered<()> {
        for (place, held, read) in write_backs {
            let ty = self.steps_type(&place)?;
            let value = self.coerce(Operand::Local(held), &ty, span)?;
            let plain = value == Operand::Local(held);
            let first = self.next_inst();
            self.write_place(place.local, &place.steps, value, span)?;
            if let (true, Some((read, block))) = (plain, read)
                && block == self.current
                && let Some(steps) = self.plain_steps(&place)
                && let Some(back) = self.read_since(first)
            {
                self.places.push(ChangedPlace { root: place.local, steps, value: held, read, back });
            }
            self.check_rules(place.local, &place.steps, span)?;
        }
        Ok(())
    }

    /// Makes what goes in a list fit, like an int in a `list[int?]`, by how the list is held.
    fn fit_list_item(&mut self, name: &str, values: &mut [Operand], span: Span) -> Lowered<()> {
        if let (Some(place), Some(Type::List(held))) =
            (item_argument(name), values.first().and_then(|list| type_in(&self.locals, list)))
            && let Some(given) = values.get(place).cloned()
        {
            let fitted = self.coerce(given, &held, span)?;
            if let Some(slot) = values.get_mut(place) {
                *slot = fitted;
            }
        }
        Ok(())
    }

    /// The standard library's `type_name[T]()`, the name of the type this copy has for `T`, and
    /// `shape_hash[T]()`, a number for its shape that a format saving no names writes and checks,
    /// and `layout[T]()`, what its values save as, with every field's name.
    fn type_called(&self, callee: &Expr) -> Option<Operand> {
        // `pack_lock()`: the kit's `pack.lock`, the same for every copy.
        if let ExprKind::Name(name) = &callee.kind
            && name == "intrinsics.pack_lock"
        {
            return Some(Operand::Str(self.context.pack_lock()));
        }
        let ExprKind::Index { object, index } = &callee.kind else { return None };
        let (ExprKind::Name(name), ExprKind::Name(param)) = (&object.kind, &index.kind) else { return None };
        let ty = self.subst.get(param)?;
        // A program's own type checked under another name, as one named like a library type is,
        // is named and shaped as it's written: what a program saves never depends on what the
        // library happens to name its own types.
        let source = || ty.source().map(|source| fennec_syntax::rename::shown_key(&source));
        let shape = || shape_hash(&fennec_syntax::rename::shown_key(&self.context.signatures.shape(ty)));
        match name.as_str() {
            "intrinsics.type_name" => source().map(Operand::Str),
            "intrinsics.shape_hash" => Some(Operand::Int(shape())),
            "intrinsics.layout" => {
                let layout = fennec_syntax::rename::shown_key(&self.context.layout(ty));
                let shape = shape();
                let name = source().unwrap_or_default().replace(' ', "");
                let known = self.context.layouts.borrow().iter().any(|saved| saved.shape == shape);
                if !known {
                    self.context.check_against_lock(ty, &name, shape);
                    let saved = crate::SavedLayout { shape, name, layout: layout.clone() };
                    self.context.layouts.borrow_mut().push(saved);
                }
                Some(Operand::Str(layout))
            }
            // Whether the kit's `pack.lock` has this type's layout as it is now, so `dumps` can
            // leave it out of what it writes.
            "intrinsics.shape_locked" => {
                let shape = shape();
                let wanted = format!("{:016x} ", shape.cast_unsigned());
                Some(Operand::Bool(self.context.pack_lock().lines().any(|line| line.starts_with(&wanted))))
            }
            _ => None,
        }
    }

    /// The name of the type an expression names, as in `Point.origin()` or `Deque[int].of(xs)`.
    fn type_named<'e>(&'e self, object: &'e Expr) -> Option<&'e str> {
        let named = match &object.kind {
            ExprKind::Index { object, .. } => object,
            _ => object,
        };
        // `T.make()`: the type this copy of a generic has for `T`.
        if let ExprKind::Name(param) = &named.kind
            && self.lookup(param).is_none()
            && let Some(ty) = self.subst.get(param)
        {
            return owner_name(ty);
        }
        match &named.kind {
            ExprKind::Name(owner) if self.names_a_type(owner) => Some(owner),
            // `pack.Unpacker.of(data)`: a type named through its module's name. `Kind.Empty` is
            // an enum's case instead.
            ExprKind::Attr { object, name: owner, optional: false }
                if matches!(&object.kind, ExprKind::Name(module) if self.lookup(module).is_none() && !self.context.enums.contains_key(module.as_str()))
                    && self.names_a_type(owner) =>
            {
                Some(owner)
            }
            _ => None,
        }
    }

    /// The type of what a method is called on.
    pub(super) fn object_type(&self, object: &Expr) -> Option<Type> {
        if let ExprKind::Name(name) = &object.kind
            && name == OPTIONAL_OBJECT
        {
            return self.lookup(name).map(|local| self.local_type(local));
        }
        self.type_at(object.span)
    }

    /// `object?.method(args)`: the call if the object isn't `None`, and `None` if it is.
    fn optional_method(
        &mut self,
        (object, name): (&Expr, &str),
        args: &[Arg],
        keep: Option<Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        let value = self.expr(object)?;
        let Some(Type::Optional(inner)) = type_in(&self.locals, &value) else {
            return Err(format!("it uses `?.{name}` on something other than an optional"));
        };
        let dest = keep.filter(|ty| *ty != Type::Unit).map(|ty| self.temporary(ty));
        let missing = self.temporary(Type::Bool);
        self.emit(InstKind::IsNone { dest: missing, value: value.clone() }, span);
        let (absent, present, join) = (self.new_block(), self.new_block(), self.new_block());
        let cond = Operand::Local(missing);
        self.finish(Terminator::Branch { cond, then: absent, otherwise: present });
        self.current = absent;
        if let Some(dest) = dest {
            self.emit(InstKind::Copy { dest, value: Operand::None }, span);
        }
        self.finish(Terminator::Jump(join));
        self.current = present;
        self.scopes.push(std::collections::HashMap::new());
        let held = self.declare(OPTIONAL_OBJECT, *inner.clone());
        self.emit(InstKind::GetSome { dest: held, value }, span);
        // It covers no source, so the typed tree says nothing about reading it.
        let at = Expr { kind: ExprKind::Name(OPTIONAL_OBJECT.to_owned()), span: Span::default() };
        let callee = Expr {
            kind: ExprKind::Attr { object: Box::new(at), name: name.to_owned(), optional: false },
            span,
        };
        let key = owner_name(&inner).map(|owner| format!("{owner}.{name}"));
        let written =
            key.and_then(|key| self.context.signatures.get(&key)).map(|found| found.returns.clone());
        let gives = match (&written, dest.map(|dest| self.local_type(dest))) {
            (Some(returns), Some(_)) => Some(returns.clone()),
            (None, Some(Type::Optional(item))) => Some(*item),
            (_, other) => other,
        };
        let result = self.call(&callee, args, gives, span);
        self.scopes.pop();
        if let (Some(dest), Some(read)) = (dest, result?) {
            let ty = self.local_type(dest);
            let read = self.coerce(Operand::Local(read), &ty, span)?;
            self.emit(InstKind::Copy { dest, value: read }, span);
        }
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(dest)
    }

    /// Whether a name is a type's rather than a value's, as in `Point.origin()`.
    fn names_a_type(&self, name: &str) -> bool {
        self.lookup(name).is_none() && !self.context.constants.contains_key(name)
    }

    /// What the engines provide that a callee names: its name, its object for a method, and
    /// whether it changes that object.
    fn builtin<'e>(&self, callee: &'e Expr) -> Option<(String, Option<&'e Expr>, bool)> {
        // A tuple's `serialize` and `deserialize` are functions the checker wrote.
        if self.tuple_saving(callee).is_some() {
            return None;
        }
        let signatures = &self.context.signatures;
        if let Some(member) = self.std_member(callee) {
            return signatures.get(&member).is_none().then_some((member, None, false));
        }
        match &callee.kind {
            ExprKind::Name(name) => {
                // What the checker accepted and nothing here defines, like an import of
                // `sqrt`, is the engines'.
                let written = signatures.get(name).is_some();
                let plain = self.lookup(name).is_none()
                    && !self.context.functions.contains(name.as_str())
                    && (fennec_check::provided_everywhere(name) || !written);
                plain.then(|| (name.clone(), None, false))
            }
            ExprKind::Attr { object, name, optional: false } => {
                if let Some(owner) = self.type_named(object) {
                    // A function on a built-in type, like `str.from_code`.
                    let key = format!("{owner}.{name}");
                    let written = signatures.get(&self.context.base_key(&key)).is_some();
                    return (!written).then_some((key, None, false));
                }
                let held = self.object_type(object)?;
                let ty = &held;
                if matches!(ty, Type::Result(..)) && name == "expect" {
                    return Some(("result.expect".to_owned(), Some(object.as_ref()), false));
                }
                // An error's notes, one pair of builtins for every `error` type that doesn't
                // write its own: `error.add_note` changes the error it's called on.
                let an_error = matches!(ty, Type::Errors(_))
                    || owner_name(ty).is_some_and(|owner| {
                        signatures.error_type(owner).is_some()
                            && signatures.get(&format!("{owner}.{name}")).is_none()
                    });
                if matches!(name.as_str(), "notes" | "add_note") && an_error {
                    return Some((format!("error.{name}"), Some(object.as_ref()), name == "add_note"));
                }
                let changes = fennec_check::builtin_method_changes(ty, name)?;
                let owner = owner_name(ty)?;
                let written = signatures.get(&format!("{owner}.{name}")).is_some();
                (!written).then(|| (format!("{owner}.{name}"), Some(object.as_ref()), changes))
            }
            _ => None,
        }
    }

    /// The key and arguments of a direct call, with one value given for each parameter. A
    /// method's object comes first. A method of a `shared` type also gives the handle whose
    /// lock the call needs, and whether it writes, unless the lock is held already.
    fn direct(
        &mut self,
        (callee, args): (&Expr, &[Arg]),
        keep: Option<&Type>,
        write_backs: &mut Vec<WriteBack>,
        span: Span,
    ) -> Lowered<(String, Vec<Operand>, Option<Lock>)> {
        let (key, object) = match &callee.kind {
            _ if let Some(found) = self.tuple_saved(callee) => found?,
            _ if self.std_member(callee).is_some() => (self.std_member(callee).unwrap_or_default(), None),
            ExprKind::Name(name) if self.lookup(name).is_none() => (name.clone(), None),
            ExprKind::Attr { object, name, optional: false } => {
                if let Some(owner) = self.type_named(object) {
                    // `Point.origin()`: a function on the type, with no object.
                    (format!("{owner}.{name}"), None)
                } else {
                    let ty = self.object_type(object).filter(|ty| self.context.holds(ty));
                    let owner = ty.as_ref().and_then(owner_name);
                    let owner = owner.ok_or_else(|| format!("it calls `.{name}` on something else"))?;
                    (format!("{owner}.{name}"), Some(object.as_ref()))
                }
            }
            _ => return Err("it calls something other than a function by name".to_owned()),
        };
        let own = self.context.functions.contains(self.context.base_key(&key).as_str());
        let raw = self.context.base_key(&key);
        let raw = self.context.signatures.get(&raw).ok_or_else(|| format!("it calls `{key}`"))?;
        let first = usize::from(object.is_some());
        let mut types: Vec<Option<Type>> = vec![None; raw.params.len()];
        if let (Some(object), Some(slot)) = (object, types.first_mut()) {
            *slot = self.object_type(object);
        }
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => raw.params.iter().position(|param| param.name == *name),
                None => Some(position + first),
            };
            if let Some(slot) = at.and_then(|at| types.get_mut(at)) {
                *slot = self.type_at(arg.value.span);
            }
        }
        let written = std::mem::take(&mut self.written_types);
        let (function, signature) = self
            .resolve_written(&key, &written, &types, keep)
            .ok_or_else(|| format!("its call to `{key}` doesn't say what its types stand for"))?;
        // The checker writes what a call leaves out into the program's own code; the standard
        // library's runs as it's written, so its calls take their defaults here.
        let defaulted = with_defaults(args, &signature.params, first);
        let args = defaulted.as_slice();
        let given = args.len() + usize::from(object.is_some());
        if given != signature.params.len() {
            return Err(format!("its call to `{key}` isn't a plain call"));
        }
        if own && !signature.params.iter().all(|param| self.context.holds(&param.ty)) {
            return Err(format!("`{key}` takes something the IR doesn't hold yet"));
        }
        let mut values: Vec<Option<Operand>> = vec![None; given];
        let mut lock = None;
        // An index may be one of the arguments changed, and the place stays where it was.
        let index_changes = args.iter().any(|arg| arg.is_mut);
        let mut later = None;
        if let Some(object) = object {
            let handle = self.object_type(object).is_some_and(|ty| self.context.is_handle(&ty));
            let changed = signature.params.first().is_some_and(|param| param.is_mut);
            // A field changed in place is read after the arguments when that's the same, as for
            // a builtin's: so nothing worked out for them holds it meanwhile.
            let after = if changed && !handle { self.read_after_arguments(object) } else { None };
            if let Some(root) = after {
                later = Some((object, root, (self.current, self.next_inst(), self.changed.len())));
            } else {
                let value = if changed && !handle {
                    self.place(object, (index_changes, false), write_backs, span)?
                } else {
                    self.expr(object)?
                };
                if handle && !self.locked_already(&value) {
                    lock = Some((value.clone(), changed));
                }
                if let Some(slot) = values.first_mut() {
                    *slot = Some(value);
                }
            }
        }
        self.direct_args((&key, own, first), &signature.params, args, (&mut values, write_backs), span)?;
        if let Some((object, root, before)) = later {
            let value = self.read_place_after(object, root, before, write_backs, span)?;
            if let Some(slot) = values.first_mut() {
                *slot = Some(value);
            }
        }
        let values: Option<Vec<Operand>> = values.into_iter().collect();
        Ok((function, values.ok_or("a parameter has no argument")?, lock))
    }

    /// A plain call's arguments, in the order they're written, each put where its parameter is.
    fn direct_args(
        &mut self,
        (key, own, first): (&str, bool, usize),
        params: &[fennec_check::ParamType],
        args: &[Arg],
        (values, write_backs): (&mut [Option<Operand>], &mut Vec<WriteBack>),
        span: Span,
    ) -> Lowered<()> {
        // An index may be one of the arguments changed, and the place stays where it was.
        let index_changes = args.iter().any(|arg| arg.is_mut);
        // Arguments run in the order they're written, and each lands in its parameter.
        let mut reads = super::order::Reads::default();
        let mut slots = Vec::with_capacity(args.len());
        // Fields changed in place, read once the rest are worked out, with where each was written.
        let mut later = Vec::new();
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => params.iter().position(|param| param.name == *name),
                None => Some(position + first),
            };
            let param = at.and_then(|at| params.get(at));
            let param = param.ok_or_else(|| format!("an argument to `{key}` matches no parameter"))?;
            let after = (param.is_mut && !self.context.is_handle(&param.ty))
                .then(|| self.read_after_arguments(&arg.value))
                .flatten();
            if let Some(root) = after {
                later.push((
                    slots.len(),
                    &arg.value,
                    root,
                    (self.current, self.next_inst(), self.changed.len()),
                ));
                reads.add_place(Operand::None);
            } else if param.is_mut && !self.context.is_handle(&param.ty) {
                let takes_optional = matches!(param.ty, Type::Optional(_));
                let place = self.place(&arg.value, (index_changes, takes_optional), write_backs, span)?;
                reads.add_place(place);
            } else {
                let lowered = self.expr(&arg.value)?;
                let value = if own { self.coerce(lowered, &param.ty, arg.value.span)? } else { lowered };
                reads.add(self, value);
            }
            slots.push(at);
        }
        let mut read = reads.finish(self, span);
        for (written_at, value, root, before) in later {
            let place = self.read_place_after(value, root, before, write_backs, span)?;
            if let Some(slot) = read.get_mut(written_at) {
                *slot = place;
            }
        }
        for (at, value) in slots.into_iter().zip(read) {
            if let Some(slot) = at.and_then(|at| values.get_mut(at)) {
                *slot = Some(value);
            }
        }
        Ok(())
    }

    /// Something a call changes: a local itself, or a temporary holding a field or item
    /// that's written back after the call. So is a name known not to be `None`, which goes
    /// back as an optional, unless what it's given to takes an optional and can make it `None`.
    /// When an index may change, the place's indexes are held apart from the names they came from.
    fn place(
        &mut self,
        expr: &Expr,
        (index_changes, takes_optional): (bool, bool),
        write_backs: &mut Vec<WriteBack>,
        span: Span,
    ) -> Lowered<Operand> {
        let place = if index_changes { self.held_place(expr, span)? } else { self.place_of(expr)? };
        self.note_changed_place(place.local);
        let held = self.steps_type(&place)?;
        let narrowed = !takes_optional
            && matches!(held, Type::Optional(_))
            && self.type_at(expr.span).is_some_and(|read| read != held);
        if place.steps.is_empty() && !narrowed {
            return Ok(Operand::Local(place.local));
        }
        let first = self.next_inst();
        let read = self.read_steps(place.local, &place.steps, span)?;
        let chain = if narrowed { None } else { self.read_since(first).map(|chain| (chain, self.current)) };
        let read = if narrowed { self.narrowed(read, expr)? } else { read };
        let Operand::Local(held) = read else {
            return Err("it changes something other than a field or item".to_owned());
        };
        write_backs.push((place, held, chain));
        Ok(Operand::Local(held))
    }

    /// Where the next instruction goes in the block being lowered.
    fn next_inst(&self) -> usize {
        self.blocks.get(self.current.0).map_or(0, |block| block.insts.len())
    }

    /// The locals read since an instruction in the block being lowered, if it's only reads of
    /// fields and items and writes of them.
    fn read_since(&self, first: usize) -> Option<Vec<LocalId>> {
        let insts = self.blocks.get(self.current.0)?.insts.get(first..)?;
        let mut read = Vec::new();
        for inst in insts {
            match &inst.kind {
                InstKind::GetField { dest, .. } | InstKind::GetIndex { dest, .. } => read.push(*dest),
                InstKind::SetField { .. } | InstKind::SetIndex { .. } => {}
                _ => return None,
            }
        }
        Some(read)
    }

    /// A place's steps, if each goes through a list or a plain struct. An optional isn't held
    /// the same way in a place as in a local, so one can't be taken out as it is. A `shared` value
    /// goes too if it's the root and its lock is held here: nothing else can read it meanwhile, as
    /// anything reaching it another way ends the program.
    fn plain_steps(&self, place: &Place) -> Option<Vec<PlaceStep>> {
        let mut ty = self.local_type(place.local);
        let mut steps = Vec::with_capacity(place.steps.len());
        let locked = self.locked_already(&Operand::Local(place.local));
        for (at, step) in place.steps.iter().enumerate() {
            steps.push(match (step, &ty) {
                (Step::Field(field), Type::Named(_))
                    if !self.context.is_handle(&ty) || (at == 0 && locked) =>
                {
                    PlaceStep::Field(field.clone())
                }
                (Step::Index(index), Type::List(_)) => PlaceStep::Item(index.clone()),
                _ => return None,
            });
            ty = self.step_type(&ty, step).ok()?;
        }
        (!matches!(ty, Type::Optional(_))).then_some(steps)
    }
}

/// Where a list's method is given an item to hold, counting the list itself.
fn item_argument(name: &str) -> Option<usize> {
    match name {
        "list.append" => Some(1),
        "list.insert" => Some(2),
        _ => None,
    }
}

/// 64-bit FNV-1a of a shape's text: the same on every machine and in every build.
fn shape_hash(shape: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in shape.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash.cast_signed()
}

/// What `list.pop()` is called on, when it's a place that can be read twice, with nothing but
/// names, fields and plain indexes on the way: `self.items` or `rows[i].tags`.
fn popped_place<'e>(callee: &'e Expr, args: &[Arg]) -> Option<&'e Expr> {
    let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return None };
    (name == "pop" && args.is_empty() && read_twice(object)).then_some(&**object)
}

fn read_twice(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(_) => true,
        ExprKind::Attr { object, optional: false, .. } => read_twice(object),
        ExprKind::Index { object, index } => {
            read_twice(object) && matches!(index.kind, ExprKind::Int(_) | ExprKind::Name(_))
        }
        _ => false,
    }
}

/// A call's arguments with the default of each parameter it leaves out, by name, after them.
fn with_defaults(args: &[Arg], params: &[fennec_check::ParamType], first: usize) -> Vec<Arg> {
    let mut all = args.to_vec();
    let positional = args.iter().filter(|arg| arg.name.is_none()).count();
    for (at, param) in params.iter().enumerate().skip(first) {
        let named = args.iter().any(|arg| arg.name.as_deref() == Some(param.name.as_str()));
        if at - first < positional || named {
            continue;
        }
        if let Some(default) = &param.default {
            all.push(Arg { name: Some(param.name.clone()), is_mut: false, value: default.clone() });
        }
    }
    all
}

/// Whether a call's type says it gives back something that could be kept.
fn gives_a_value(ty: &Type) -> bool {
    !matches!(ty, Type::Unit | Type::Unknown | Type::None)
}

/// The intrinsics the IR lowers itself, rather than leave to the backend as they are.
const LOWERED: [&str; 6] = [
    "intrinsics.abandoning",
    "intrinsics.channel_receive",
    "intrinsics.launch_end",
    "intrinsics.launch",
    "intrinsics.crash_cleanup",
    "intrinsics.crash_cleanup_done",
];
