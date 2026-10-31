//! Checks the IR is well formed, so a mistake in lowering is caught here and not as a
//! miscompiled program. Passing says nothing about what the source meant, only that every
//! local and block exists and every instruction's types line up.

use fennec_check::Type;
use fennec_syntax::{BinOp, UnaryOp};

use crate::{FormatPart, Function, InstKind, LocalId, Operand, Program, StructDef, Terminator};

/// Every way a program's IR is malformed, each naming its function. Empty when it's sound.
pub fn validate(program: &Program) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    for (at, function) in program.functions.iter().enumerate() {
        if program.functions.iter().take(at).any(|earlier| earlier.name == function.name) {
            problems.push(format!("{}: defined twice", function.name));
        }
    }
    for function in program.functions.iter().chain(&program.constants) {
        let mut check = Check { program, function, problems: Vec::new() };
        check.function();
        problems.extend(check.problems.into_iter().map(|problem| format!("{}: {problem}", function.name)));
    }
    if problems.is_empty() { Ok(()) } else { Err(problems) }
}

struct Check<'p> {
    program: &'p Program,
    function: &'p Function,
    problems: Vec<String>,
}

impl<'p> Check<'p> {
    fn function(&mut self) {
        let function = self.function;
        if function.params > function.locals.len() {
            self.problems.push(format!("{} parameters but fewer locals", function.params));
        }
        if function.blocks.is_empty() {
            self.problems.push("no blocks".to_owned());
        }
        for block in &function.blocks {
            for inst in &block.insts {
                self.inst(&inst.kind);
            }
            self.terminator(&block.end);
        }
    }

    fn inst(&mut self, inst: &InstKind) {
        match inst {
            InstKind::Copy { dest, value } => {
                let wanted = self.local(*dest);
                self.operand(value, wanted.as_ref(), "copy");
            }
            InstKind::Unary { dest, op, value } => {
                let wanted = self.local(*dest);
                let allowed = match op {
                    UnaryOp::Not => matches!(wanted, Some(Type::Bool)),
                    UnaryOp::Neg => matches!(wanted, Some(Type::Int | Type::Float)),
                };
                if !allowed {
                    self.problems.push(format!("`{inst}` on the wrong type"));
                }
                self.operand(value, wanted.as_ref(), "unary operator");
            }
            InstKind::Binary { dest, op, lhs, rhs } => self.binary(inst, *dest, *op, (lhs, rhs)),
            InstKind::Make { dest, type_name, fields } => self.make(*dest, type_name, fields),
            InstKind::GetField { dest, object, field } => {
                let wanted = self.local(*dest);
                self.operand(object, None, "object");
                let found = self.function.type_of(object).and_then(|ty| self.field(&ty, field));
                if found.is_none() || found != wanted {
                    self.problems.push(format!("`{inst}` reads the wrong field or type"));
                }
            }
            InstKind::SetField { object, path, value } => {
                let mut ty = self.local(*object);
                for field in path {
                    ty = ty.and_then(|ty| self.field(&ty, field));
                }
                if ty.is_none() {
                    self.problems.push(format!("`{inst}` changes a field that doesn't exist"));
                }
                self.operand(value, ty.as_ref(), "field value");
            }
            InstKind::MakeCase { dest, type_name, case, fields } => {
                self.make_case(*dest, type_name, case, fields);
            }
            InstKind::IsCase { dest, value, case } => {
                if self.local(*dest) != Some(Type::Bool) || self.case_fields(value, case).is_none() {
                    self.problems.push(format!("`{inst}` tests the wrong thing"));
                }
            }
            InstKind::GetCaseField { dest, value, case, field } => {
                let wanted = self.local(*dest);
                let fields = self.case_fields(value, case).unwrap_or_default();
                let found = fields.iter().find(|(name, _)| name == field).map(|(_, ty)| ty.clone());
                if found.is_none() || found != wanted {
                    self.problems.push(format!("`{inst}` reads the wrong field or type"));
                }
            }
            InstKind::MakeList { .. }
            | InstKind::MakeSet { .. }
            | InstKind::MakeDict { .. }
            | InstKind::MakeTuple { .. }
            | InstKind::GetTupleItem { .. }
            | InstKind::GetIndex { .. }
            | InstKind::Slice { .. }
            | InstKind::SetIndex { .. } => self.collection_inst(inst),
            InstKind::Format { .. } | InstKind::Builtin { .. } => self.engine_inst(inst),
            InstKind::Call { dest, function, args } => self.call(*dest, function, args),
            InstKind::Constant { dest, name } => self.constant(*dest, name),
            InstKind::MakeClosure { dest, function, captures } => {
                self.make_closure(*dest, function, captures);
            }
            InstKind::CallClosure { dest, closure, args } => self.call_closure(*dest, closure, args),
            _ => self.failure_inst(inst),
        }
    }

    /// Optionals and results.
    fn failure_inst(&mut self, inst: &InstKind) {
        let held = |check: &Self, value: &Operand| check.function.type_of(value);
        let fine = match inst {
            InstKind::MakeSome { dest, value } => match self.local(*dest) {
                Some(Type::Optional(inner)) => held(self, value).is_some_and(|ty| fits(&inner, &ty)),
                _ => false,
            },
            InstKind::GetSome { dest, value } => match held(self, value) {
                Some(Type::Optional(inner)) => self.local(*dest) == Some(*inner),
                _ => false,
            },
            InstKind::MakeOk { dest, value } => match (self.local(*dest), value) {
                (Some(Type::Result(ok, _)), Some(value)) => {
                    held(self, value).is_some_and(|ty| fits(&ok, &ty))
                }
                (Some(Type::Result(ok, _)), None) => *ok == Type::Unit,
                _ => false,
            },
            InstKind::MakeErr { dest, error } => match self.local(*dest) {
                Some(Type::Result(_, wanted)) => held(self, error).is_some_and(|ty| fits(&wanted, &ty)),
                _ => false,
            },
            InstKind::GetOk { dest, value } => match held(self, value) {
                Some(Type::Result(ok, _)) => self.local(*dest) == Some(*ok),
                _ => false,
            },
            InstKind::GetErr { dest, value } => match held(self, value) {
                Some(Type::Result(_, error)) => self.local(*dest) == Some(*error),
                _ => false,
            },
            InstKind::IsNone { dest, value } => {
                self.local(*dest) == Some(Type::Bool) && matches!(held(self, value), Some(Type::Optional(_)))
            }
            InstKind::IsErr { dest, value } => {
                self.local(*dest) == Some(Type::Bool) && matches!(held(self, value), Some(Type::Result(..)))
            }
            InstKind::IsError { dest, value, .. } => {
                self.local(*dest) == Some(Type::Bool) && matches!(held(self, value), Some(Type::Errors(_)))
            }
            InstKind::AsError { dest, value } => match (self.local(*dest), held(self, value)) {
                (Some(Type::Named(name)), Some(Type::Errors(names))) => names.contains(&name),
                _ => false,
            },
            _ => true,
        };
        if !fine {
            self.problems.push(format!("`{inst}` has the wrong types"));
        }
    }

    /// Building collections, and reading and changing their items.
    fn collection_inst(&mut self, inst: &InstKind) {
        match inst {
            InstKind::MakeList { dest, items } => match self.local(*dest) {
                Some(Type::List(item)) => self.operands(items, &item),
                _ => self.problems.push(format!("`{inst}` makes a list in another type")),
            },
            InstKind::MakeSet { dest, items } => match self.local(*dest) {
                Some(Type::Set(item)) => self.operands(items, &item),
                _ => self.problems.push(format!("`{inst}` makes a set in another type")),
            },
            InstKind::MakeDict { dest, pairs } => {
                let Some(Type::Dict(key, value)) = self.local(*dest) else {
                    self.problems.push(format!("`{inst}` makes a dict in another type"));
                    return;
                };
                for (written, item) in pairs {
                    self.operand(written, Some(&key), "key");
                    self.operand(item, Some(&value), "value");
                }
            }
            InstKind::MakeTuple { dest, items } => {
                let Some(Type::Tuple(wanted)) = self.local(*dest) else {
                    self.problems.push(format!("`{inst}` makes a tuple in another type"));
                    return;
                };
                if wanted.len() != items.len() {
                    self.problems.push(format!("`{inst}` makes a tuple of another size"));
                }
                for (item, ty) in items.iter().zip(&wanted) {
                    self.operand(item, Some(ty), "item");
                }
            }
            InstKind::GetTupleItem { dest, value, at } => {
                let found = match self.function.type_of(value) {
                    Some(Type::Tuple(items)) => items.get(*at).cloned(),
                    _ => None,
                };
                if found.is_none() || found != self.local(*dest) {
                    self.problems.push(format!("`{inst}` reads the wrong thing"));
                }
            }
            InstKind::GetIndex { dest, object, index } => {
                let wanted = self.local(*dest);
                let found = self.function.type_of(object).as_ref().and_then(indexed);
                if let Some((key, _)) = found.filter(|(_, item)| Some(item) == wanted.as_ref()) {
                    self.operand(index, Some(&key), "index");
                } else {
                    self.problems.push(format!("`{inst}` reads the wrong thing"));
                }
            }
            InstKind::Slice { dest, object, start, end } => {
                let held = self.function.type_of(object);
                let sliced = matches!(held, Some(Type::List(_) | Type::Str | Type::Bytes));
                if !sliced || held != self.local(*dest) {
                    self.problems.push(format!("`{inst}` slices the wrong thing"));
                }
                for bound in start.iter().chain(end) {
                    self.operand(bound, Some(&Type::Int), "bound");
                }
            }
            InstKind::SetIndex { object, path, index, value } => {
                let mut ty = self.local(*object);
                for field in path {
                    ty = ty.and_then(|ty| self.field(&ty, field));
                }
                let (key, item) = match ty {
                    Some(Type::List(item)) => (Type::Int, *item),
                    Some(Type::Dict(key, item)) => (*key, *item),
                    Some(Type::IdMap(item)) => (Type::Id(item.clone()), *item),
                    _ => {
                        self.problems.push(format!("`{inst}` changes an item of something else"));
                        return;
                    }
                };
                self.operand(index, Some(&key), "index");
                self.operand(value, Some(&item), "item");
            }
            _ => {}
        }
    }

    fn operands(&mut self, items: &[Operand], wanted: &Type) {
        for value in items {
            self.operand(value, Some(wanted), "item");
        }
    }

    /// F-strings and what the engines provide.
    fn engine_inst(&mut self, inst: &InstKind) {
        match inst {
            InstKind::Format { dest, parts } => {
                if self.local(*dest) != Some(Type::Str) {
                    self.problems.push(format!("`{inst}` keeps text in another type"));
                }
                for part in parts {
                    if let FormatPart::Value { value, .. } = part {
                        self.operand(value, None, "value");
                    }
                }
            }
            InstKind::Builtin { dest, args, changes, .. } => {
                if let Some(dest) = dest {
                    self.local(*dest);
                }
                for arg in args {
                    self.operand(arg, None, "argument");
                }
                if *changes && !matches!(args.first(), Some(Operand::Local(_))) {
                    self.problems.push(format!("`{inst}` changes something other than a local"));
                }
            }
            _ => {}
        }
    }

    /// The fields of a case of the enum a value is of.
    fn case_fields(&self, value: &Operand, case: &str) -> Option<Vec<(String, Type)>> {
        let Some(Type::Named(name)) = self.function.type_of(value) else { return None };
        let def = self.program.enums.iter().find(|def| def.name == name)?;
        def.cases.iter().find(|written| written.name == case).map(|found| found.fields.clone())
    }

    fn make_case(&mut self, dest: LocalId, type_name: &str, case: &str, fields: &[Operand]) {
        if self.local(dest) != Some(Type::Named(type_name.to_owned())) {
            self.problems.push(format!("keeps a new `{type_name}` in the wrong type"));
        }
        let def = self.program.enums.iter().find(|def| def.name == type_name);
        let Some(found) = def.and_then(|def| def.cases.iter().find(|written| written.name == case)) else {
            self.problems.push(format!("makes `{type_name}.{case}`, which doesn't exist"));
            return;
        };
        if fields.len() != found.fields.len() {
            self.problems.push(format!("makes `{type_name}.{case}` with {} fields", fields.len()));
        }
        for (value, (_, ty)) in fields.iter().zip(&found.fields) {
            self.operand(value, Some(ty), "field value");
        }
    }

    /// A struct or error type, which both hold named fields.
    fn struct_like(&self, name: &str) -> Option<&'p StructDef> {
        let program = self.program;
        program.structs.iter().chain(&program.errors).find(|def| def.name == name)
    }

    /// The type of a field of a struct type.
    fn field(&self, ty: &Type, field: &str) -> Option<Type> {
        let Type::Named(name) = ty else { return None };
        let def = self.struct_like(name)?;
        def.fields.iter().find(|(written, _)| written == field).map(|(_, ty)| ty.clone())
    }

    fn make(&mut self, dest: LocalId, type_name: &str, fields: &[Operand]) {
        if self.local(dest) != Some(Type::Named(type_name.to_owned())) {
            self.problems.push(format!("keeps a new `{type_name}` in the wrong type"));
        }
        let Some(def) = self.struct_like(type_name) else {
            self.problems.push(format!("makes `{type_name}`, which doesn't exist"));
            return;
        };
        if fields.len() != def.fields.len() {
            self.problems.push(format!("makes `{type_name}` with {} fields", fields.len()));
        }
        for (value, (_, ty)) in fields.iter().zip(&def.fields) {
            self.operand(value, Some(ty), "field value");
        }
    }

    fn binary(&mut self, inst: &InstKind, dest: LocalId, op: BinOp, (lhs, rhs): (&Operand, &Operand)) {
        let result = self.local(dest);
        let left = self.function.type_of(lhs);
        self.operand(lhs, left.as_ref(), "left side");
        self.operand(rhs, left.as_ref(), "right side");
        let Some(left) = left else { return };
        let wanted = match op {
            BinOp::Eq | BinOp::NotEq => Some(Type::Bool),
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq if plainly_ordered(&left) => Some(Type::Bool),
            BinOp::Div if left == Type::Int => Some(Type::Float),
            BinOp::Add if matches!(left, Type::Str | Type::Bytes | Type::List(_)) => Some(left.clone()),
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod | BinOp::Pow
                if matches!(left, Type::Int | Type::Float) =>
            {
                Some(left.clone())
            }
            _ => None,
        };
        if wanted.is_none() || wanted != result {
            self.problems.push(format!("`{inst}` on the wrong types"));
        }
    }

    fn make_closure(&mut self, dest: LocalId, callee: &str, captures: &[Operand]) {
        let Some(Type::Arrow(params, returns)) = self.local(dest) else {
            self.problems.push(format!("keeps a closure of `{callee}` in another type"));
            return;
        };
        let Some(target) = self.program.functions.iter().find(|function| function.name == callee) else {
            let elsewhere = self.program.external.iter().any(|name| name == callee)
                || self.program.not_lowered.iter().any(|skipped| skipped.function == callee);
            if !elsewhere {
                self.problems.push(format!("makes a closure of `{callee}`, which doesn't exist"));
            }
            return;
        };
        if target.params != captures.len() + params.len() || target.returns != *returns {
            self.problems.push(format!("makes a closure of `{callee}` of the wrong shape"));
        }
        let wanted = captures.iter().map(Some).chain(params.iter().map(|_| None));
        for (captured, param) in wanted.zip(&target.locals) {
            if let Some(captured) = captured {
                self.operand(captured, Some(&param.ty), "capture");
            }
        }
        for (param, local) in params.iter().zip(target.locals.iter().skip(captures.len())) {
            if *param != local.ty {
                self.problems.push(format!("makes a closure of `{callee}` taking the wrong types"));
            }
        }
    }

    fn call_closure(&mut self, dest: Option<LocalId>, closure: &Operand, args: &[Operand]) {
        let Some(Type::Arrow(params, returns)) = self.function.type_of(closure) else {
            self.problems.push(format!("calls `{closure}`, which isn't a closure"));
            return;
        };
        if args.len() != params.len() {
            self.problems.push(format!("calls `{closure}` with {} arguments", args.len()));
        }
        for (arg, param) in args.iter().zip(&params) {
            self.operand(arg, Some(param), "argument");
        }
        if let Some(dest) = dest
            && self.local(dest).as_ref() != Some(&*returns)
        {
            self.problems.push(format!("keeps what `{closure}` gives back in the wrong type"));
        }
    }

    fn constant(&mut self, dest: LocalId, name: &str) {
        let wanted = self.local(dest);
        let found = self.program.constants.iter().find(|constant| constant.name == name);
        match found {
            Some(constant) if wanted.as_ref() != Some(&constant.returns) => {
                self.problems.push(format!("keeps constant `{name}` in the wrong type"));
            }
            None if !self.program.not_lowered.iter().any(|skipped| skipped.function == name) => {
                self.problems.push(format!("reads constant `{name}`, which doesn't exist"));
            }
            _ => {}
        }
    }

    fn call(&mut self, dest: Option<LocalId>, callee: &str, args: &[Operand]) {
        if let Some(dest) = dest {
            self.local(dest);
        }
        for arg in args {
            self.operand(arg, None, "argument");
        }
        let Some(target) = self.program.functions.iter().find(|function| function.name == callee) else {
            let elsewhere = self.program.external.iter().any(|name| name == callee)
                || self.program.not_lowered.iter().any(|skipped| skipped.function == callee);
            if !elsewhere {
                self.problems.push(format!("calls `{callee}`, which doesn't exist"));
            }
            return;
        };
        if args.len() != target.params {
            self.problems.push(format!("calls `{callee}` with {} arguments", args.len()));
        }
        for (arg, param) in args.iter().zip(&target.locals) {
            self.operand(arg, Some(&param.ty), "argument");
        }
        for at in &target.mut_params {
            if !matches!(args.get(*at), Some(Operand::Local(_))) {
                self.problems.push(format!("passes something other than a local to `{callee}`'s `mut`"));
            }
        }
        if let Some(dest) = dest
            && self.local(dest).is_some_and(|ty| ty != target.returns)
        {
            self.problems.push(format!("keeps what `{callee}` gives back in the wrong type"));
        }
    }

    fn terminator(&mut self, end: &Terminator) {
        for target in end.targets() {
            if self.function.blocks.get(target.0).is_none() {
                self.problems.push(format!("goes to {target}, which doesn't exist"));
            }
        }
        match end {
            Terminator::Branch { cond, .. } => self.operand(cond, Some(&Type::Bool), "condition"),
            Terminator::Return(Some(value)) => {
                let returns = self.function.returns.clone();
                self.operand(value, Some(&returns), "return");
            }
            Terminator::Return(None) if self.function.returns != Type::Unit => {
                self.problems.push("returns nothing, but has a return type".to_owned());
            }
            Terminator::NoMatch { value, .. } => self.operand(value, None, "value"),
            Terminator::Return(None)
            | Terminator::Jump(_)
            | Terminator::Stop { .. }
            | Terminator::Unreachable => {}
        }
    }

    /// The type of a local this function has.
    fn local(&mut self, id: LocalId) -> Option<Type> {
        let ty = self.function.locals.get(id.0).map(|local| local.ty.clone());
        if ty.is_none() {
            self.problems.push(format!("uses {id}, which doesn't exist"));
        }
        ty
    }

    /// Checks a value exists and, when one is wanted, has that type.
    fn operand(&mut self, operand: &Operand, wanted: Option<&Type>, what: &str) {
        match self.function.type_of(operand) {
            None => self.problems.push(format!("uses {operand}, which doesn't exist")),
            Some(ty) if wanted.is_some_and(|wanted| !fits(wanted, &ty)) => {
                self.problems.push(format!("{what} `{operand}` has the wrong type"));
            }
            Some(_) => {}
        }
    }
}

/// Whether `<` and so on on a type are the engines' own, not a method.
pub(crate) fn plainly_ordered(ty: &Type) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Str | Type::Bytes => true,
        Type::List(item) => plainly_ordered(item),
        Type::Tuple(items) => items.iter().all(plainly_ordered),
        _ => false,
    }
}

/// What a value of this type is indexed by, and what that gives.
fn indexed(ty: &Type) -> Option<(Type, Type)> {
    match ty {
        Type::List(item) => Some((Type::Int, *item.clone())),
        Type::Str => Some((Type::Int, Type::Str)),
        Type::Bytes => Some((Type::Int, Type::Int)),
        Type::Dict(key, value) => Some((*key.clone(), *value.clone())),
        Type::Named(name) if name == "Args" => Some((Type::Int, Type::Str)),
        Type::IdMap(item) => Some((Type::Id(item.clone()), *item.clone())),
        Type::Tuple(items) => {
            let first = items.first()?;
            items.iter().all(|item| item == first).then(|| (Type::Int, first.clone()))
        }
        _ => None,
    }
}

/// Whether a value of type `actual` can go where `wanted` goes as it is: `None` where an
/// optional goes, and errors where a set of error types that includes them goes.
pub(crate) fn fits(wanted: &Type, actual: &Type) -> bool {
    match (wanted, actual) {
        (Type::Optional(_), Type::None) => true,
        (Type::Errors(names), Type::Named(name)) => names.contains(name),
        (Type::Errors(names), Type::Errors(some)) => some.iter().all(|name| names.contains(name)),
        (Type::Result(ok, wanted), Type::Result(same, error)) => ok == same && fits(wanted, error),
        _ => wanted == actual,
    }
}
