//! Calls: to the program's functions and methods, to the standard library's written in
//! Fennec, and to what the engines provide. An argument to a `mut` parameter that's a field
//! is read into a temporary and written back after the call.

use fennec_check::Type;
use fennec_syntax::{Arg, Expr, ExprKind, Span};

use super::places::{Place, Step};
use super::{Lowered, Lowering};
use crate::{BlockId, ChangedPlace, InstKind, LocalId, Operand, PlaceStep, Program, Terminator, type_in};

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
        Type::Buffer(_) => Some("Buffer"),
        Type::Set(_) => Some("set"),
        Type::Dict(..) => Some("dict"),
        Type::Tuple(_) => Some("tuple"),
        Type::Bytes => Some("bytes"),
        Type::IdMap(_) => Some("IdMap"),
        Type::Str => Some("str"),
        Type::Int => Some("int"),
        Type::Float => Some("float"),
        Type::Bool => Some("bool"),
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
        if let Some(object) = object {
            if changes {
                let place = self.place(object, (false, false), write_backs, span)?;
                reads.add_place(place);
            } else {
                let value = self.expr(object)?;
                reads.add(self, value);
            }
        }
        let mut written = Vec::with_capacity(args.len());
        for arg in args {
            if arg.is_mut {
                return Err(format!("its call to `{name}` has a `mut` argument"));
            }
            let value = match &arg.value.kind {
                ExprKind::Lambda { params, body } if name == "IdMap.modify" => {
                    self.arrow(&arg.value, params, body, true)?
                }
                // What's printed, or made text, through a type's own `def str`.
                _ if matches!(name.as_str(), "Stdout.print" | "Stdout.write" | "str") => {
                    self.shown(&arg.value)?
                }
                _ => self.expr(&arg.value)?,
            };
            reads.add(self, value);
            written.push(arg.name.clone());
        }
        let mut read = reads.finish(self, span).into_iter();
        let mut values: Vec<Operand> = object.iter().filter_map(|_| read.next()).collect();
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

    /// Lowers a call. When it's given the type of what the call gives back, that's kept in a
    /// new temporary.
    pub(super) fn call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        keep: Option<Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        if let Some(ty) = self.closure_callee(callee) {
            return self.call_closure(callee, &ty, args, keep, span);
        }
        if let ExprKind::Attr { object, name, optional: true } = &callee.kind {
            return self.optional_method((object, name), args, keep, span);
        }
        // `json.loads[T](text)`: the type it's given is the type of what it gives back.
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
            let values = self.builtin_args((&mut name, object, changes), args, &mut write_backs, span)?;
            dest = keep.map(|ty| self.temporary(ty));
            self.emit(InstKind::Builtin { dest, name, args: values, changes }, span);
        } else {
            let (function, values, lock) =
                self.direct((callee, args), keep.as_ref(), &mut write_backs, span)?;
            dest = keep.map(|ty| self.temporary(ty));
            if self.context.intrinsics.contains(&function) {
                self.emit(InstKind::Builtin { dest, name: function, args: values, changes: false }, span);
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
            _ => None,
        }
    }

    /// The type of what a method is called on.
    fn object_type(&self, object: &Expr) -> Option<Type> {
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
                    return signatures.get(&key).is_none().then_some((key, None, false));
                }
                let held = self.object_type(object)?;
                let ty = &held;
                if matches!(ty, Type::Result(..)) && name == "expect" {
                    return Some(("result.expect".to_owned(), Some(object.as_ref()), false));
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
        let (function, signature) = self
            .resolve(&key, &types, keep)
            .ok_or_else(|| format!("its call to `{key}` doesn't say what its types stand for"))?;
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
        if let Some(object) = object {
            let handle = self.object_type(object).is_some_and(|ty| self.context.is_handle(&ty));
            let changed = signature.params.first().is_some_and(|param| param.is_mut);
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
        self.direct_args((&key, own, first), &signature.params, args, (&mut values, write_backs), span)?;
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
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => params.iter().position(|param| param.name == *name),
                None => Some(position + first),
            };
            let param = at.and_then(|at| params.get(at));
            let param = param.ok_or_else(|| format!("an argument to `{key}` matches no parameter"))?;
            if param.is_mut && !self.context.is_handle(&param.ty) {
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
        for (at, value) in slots.into_iter().zip(reads.finish(self, span)) {
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
    /// the same way in a place as in a local, so one can't be taken out as it is.
    fn plain_steps(&self, place: &Place) -> Option<Vec<PlaceStep>> {
        let mut ty = self.local_type(place.local);
        let mut steps = Vec::with_capacity(place.steps.len());
        for step in &place.steps {
            steps.push(match (step, &ty) {
                (Step::Field(field), Type::Named(_)) if !self.context.is_handle(&ty) => {
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
