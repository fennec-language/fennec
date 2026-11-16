//! Working out constants while compiling: a small evaluator over the IR, so a constant like
//! `SINE = [sin(float(i) * PI / 512.0) for i in range(1024)]` is baked into the program as the
//! literal of its value.
//!
//! It runs the functions lowering gives each constant, and the functions they call, one
//! instruction at a time within a budget of steps. It knows the operators, the collections and
//! a set of the engines' builtins. Whatever else a constant reaches, it leaves for the program
//! to work out when it starts, as natively built code does for any constant that isn't a
//! literal: it never guesses at what the engines would do. A constant that fails here fails the
//! same way every time the program starts, so it's a compile error.

#![expect(clippy::too_many_lines, reason = "one match runs each instruction, and one each builtin")]

mod builtins;
mod literal;
mod text;
mod value;

use std::collections::HashMap;
use std::rc::Rc;

use fennec_syntax::{BinOp, Diagnostic, Span, UnaryOp};

use crate::lengths::{Length, Lengths};
use crate::ranges::{Fact, Ranges};
use crate::{Function, InstKind, LocalId, Operand, Program, Terminator};

pub use literal::{Written, literal};
pub use value::Value;

/// What working out one constant gave.
#[derive(Debug)]
pub enum Worked {
    Value(Value),
    /// It took more steps than the budget, so it's worked out when the program starts.
    TooLong,
    /// It reaches something this evaluator doesn't do yet, named here, so it's worked out when
    /// the program starts.
    NotHere(String),
    /// It failed, as it would every time the program started.
    Failed(Diagnostic),
}

/// Works out each constant in `program.constants`, each within `steps` steps. One that reads
/// another works that one out too, within its own budget, and keeps it.
pub fn work_out(program: &Program, steps: u64) -> Vec<(String, Worked)> {
    let mut machine = Machine::new(program);
    let mut worked = Vec::new();
    for constant in &program.constants {
        machine.steps_left = steps;
        let outcome = match machine.constant(&constant.name, Span::default()) {
            Ok(value) => Worked::Value(value),
            Err(Stop::TooLong) => Worked::TooLong,
            Err(Stop::NotHere(what)) => Worked::NotHere(what),
            Err(Stop::Failed(problem)) => Worked::Failed(problem),
            Err(Stop::Broken(why)) => Worked::NotHere(why),
        };
        worked.push((constant.name.clone(), outcome));
    }
    worked
}

/// What running a program with what's known of its list lengths checked found.
#[derive(Debug)]
pub struct LengthsChecked {
    /// How many times a fact was checked: on the way into a function, and on the way out.
    pub checked: u64,
    /// A fact that didn't hold, said plainly.
    pub broken: Option<String>,
    /// Why the run ended before `main` did: too many steps, a failure, or something this
    /// evaluator doesn't do.
    pub stopped: Option<String>,
}

/// Runs a program's `main` within `steps` steps, checking each fact `lengths` gives about a
/// function's list parameters on the way into it and out of it, and about the list it gives
/// back. What `main` is given is a stand-in, and printing does nothing, so the run goes as far
/// as this evaluator can take it. It's a test of the facts, not of the program.
pub fn check_lengths(program: &Program, lengths: &Lengths, steps: u64) -> LengthsChecked {
    let mut machine = Machine::new(program);
    machine.lengths = Some(lengths);
    machine.quiet = true;
    machine.steps_left = steps;
    let ran = match machine.functions.get("main").copied() {
        Some(main) => machine.call(main, vec![Value::Unit; main.params]).map(|_| ()),
        None => not_here("a program with no `main`"),
    };
    let (broken, stopped) = match ran {
        Ok(()) => (None, None),
        Err(Stop::Broken(why)) => (Some(why), None),
        Err(Stop::TooLong) => (None, Some("too many steps".to_owned())),
        Err(Stop::NotHere(what)) => (None, Some(what)),
        Err(Stop::Failed(problem)) => (None, Some(problem.message)),
    };
    LengthsChecked { checked: machine.checked, broken, stopped }
}

/// Runs a program's `main` within `steps` steps, checking each fact `ranges` gives: what each
/// value written to a local and each parameter given holds, and the operands of each `//` and
/// `%` said unsigned, each `int(x)` said to fit and each comparison said not NaN, as the program
/// runs. As with [`check_lengths`], it's a test of the facts, not of the program.
pub fn check_ranges(program: &Program, ranges: &Ranges, steps: u64) -> LengthsChecked {
    let mut machine = Machine::new(program);
    machine.ranges = Some(ranges);
    machine.quiet = true;
    machine.steps_left = steps;
    let ran = match machine.functions.get("main").copied() {
        Some(main) => machine.call(main, vec![Value::Unit; main.params]).map(|_| ()),
        None => not_here("a program with no `main`"),
    };
    let (broken, stopped) = match ran {
        Ok(()) => (None, None),
        Err(Stop::Broken(why)) => (Some(why), None),
        Err(Stop::TooLong) => (None, Some("too many steps".to_owned())),
        Err(Stop::NotHere(what)) => (None, Some(what)),
        Err(Stop::Failed(problem)) => (None, Some(problem.message)),
    };
    LengthsChecked { checked: machine.checked, broken, stopped }
}

/// Whether a value holds what's known about it. A value of a kind the fact doesn't say anything
/// about, as a local's before it's written, holds it.
fn holds(value: &Value, fact: &Fact) -> bool {
    match (fact, value) {
        (Fact::Any, _) | (_, Value::Unit) => true,
        (Fact::Unset, _) => false,
        (Fact::Int(low, high), Value::Int(number)) => low <= number && number <= high,
        (Fact::Float(known), Value::Float(number)) => {
            if number.is_nan() {
                known.nan
            } else {
                known.low <= *number && *number <= known.high
            }
        }
        (Fact::Struct(fields), Value::Instance(instance)) => {
            instance.fields.len() == fields.len()
                && instance.fields.iter().zip(fields).all(|(value, fact)| holds(value, fact))
        }
        (Fact::Tuple(items), Value::Tuple(values)) => {
            values.len() == items.len() && values.iter().zip(items).all(|(value, fact)| holds(value, fact))
        }
        (Fact::List(items), Value::List(values)) => values.iter().all(|value| holds(value, items)),
        _ => true,
    }
}

/// The length of a list.
fn length_of(value: &Value) -> Option<usize> {
    match value {
        Value::List(items) => Some(items.len()),
        _ => None,
    }
}

/// Why working something out stopped.
#[derive(Debug)]
pub(crate) enum Stop {
    TooLong,
    NotHere(String),
    Failed(Diagnostic),
    /// A fact about a list's length didn't hold.
    Broken(String),
}

pub(crate) type Ran<T> = Result<T, Stop>;

/// Stops on something this evaluator doesn't do.
pub(crate) fn not_here<T>(what: impl Into<String>) -> Ran<T> {
    Err(Stop::NotHere(what.into()))
}

/// Stops on a failure, as the program would.
pub(crate) fn fail<T>(message: impl Into<String>, span: Span) -> Ran<T> {
    Err(Stop::Failed(Diagnostic::new(message, span)))
}

/// How deep calls may go here. Deeper is worked out when the program starts, whose stack is
/// the program's own.
const DEEPEST: usize = 2_000;

/// A constant's state: worked out, or being worked out now.
enum Known {
    Working,
    Done(Value),
}

/// The functions, the constants worked out so far, and the steps left.
pub(crate) struct Machine<'p> {
    program: &'p Program,
    functions: HashMap<&'p str, &'p Function>,
    constants: HashMap<&'p str, &'p Function>,
    known: HashMap<String, Known>,
    /// Each struct's, error's and enum case's field names, in order, by `Type` or `Type.Case`.
    fields: HashMap<String, Vec<&'p str>>,
    steps_left: u64,
    depth: usize,
    /// What's known of list lengths, checked as the program runs, when it's being checked.
    lengths: Option<&'p Lengths>,
    /// What's known of ints' and floats' values, checked as the program runs, when it's being
    /// checked.
    ranges: Option<&'p Ranges>,
    /// Whether printing does nothing, rather than stopping here.
    quiet: bool,
    /// How many times a length was checked.
    checked: u64,
}

impl<'p> Machine<'p> {
    fn new(program: &'p Program) -> Self {
        let mut fields = HashMap::new();
        for def in program.structs.iter().chain(&program.errors) {
            fields.insert(def.name.clone(), def.fields.iter().map(|(name, _)| name.as_str()).collect());
        }
        for def in &program.enums {
            for case in &def.cases {
                let names = case.fields.iter().map(|(name, _)| name.as_str()).collect();
                fields.insert(format!("{}.{}", def.name, case.name), names);
            }
        }
        Machine {
            program,
            functions: program.functions.iter().map(|function| (function.name.as_str(), function)).collect(),
            constants: program.constants.iter().map(|function| (function.name.as_str(), function)).collect(),
            known: HashMap::new(),
            fields,
            steps_left: 0,
            depth: 0,
            lengths: None,
            ranges: None,
            quiet: false,
            checked: 0,
        }
    }

    /// What's known about a function's values, when they're being checked.
    fn ranges_of(&self, function: &Function) -> Option<&'p crate::ranges::FunctionRanges> {
        self.ranges.and_then(|ranges| ranges.function(&function.name))
    }

    /// Checks what's known of a function's parameters that aren't lists against what it's given.
    fn check_given(&mut self, function: &Function, locals: &[Value]) -> Ran<()> {
        let Some(known) = self.ranges_of(function) else { return Ok(()) };
        for at in 0..function.params {
            let (Some(fact), Some(value)) = (known.local(LocalId(at)), locals.get(at)) else { continue };
            self.checked += 1;
            if !holds(value, fact) {
                return Err(Stop::Broken(format!(
                    "`{}` was given {value:?} for parameter {at}, where {fact:?} says",
                    function.name
                )));
            }
        }
        Ok(())
    }

    /// Checks the operands of an instruction a check is left out of because of what's known.
    fn check_operands(
        &mut self,
        function: &Function,
        place: crate::ranges::Place,
        inst: &InstKind,
        locals: &[Value],
    ) -> Ran<()> {
        let Some(known) = self.ranges_of(function) else { return Ok(()) };
        let broken = match inst {
            InstKind::Binary { op: BinOp::FloorDiv | BinOp::Mod, lhs, rhs, .. } if known.unsigned(place) => {
                self.checked += 1;
                let sides = (read(locals, lhs), read(locals, rhs));
                (!matches!(sides, (Value::Int(left), Value::Int(right)) if left >= 0 && right >= 0))
                    .then(|| format!("{sides:?} isn't unsigned"))
            }
            InstKind::Builtin { args, .. } if known.fits(place) => {
                self.checked += 1;
                let given = args.first().map(|arg| read(locals, arg));
                let fits = matches!(given, Some(Value::Float(number)) if number.is_finite() && number.abs() < 9.223_372_036_854_775e18);
                (!fits).then(|| format!("`int` was given {given:?}, which doesn't fit"))
            }
            InstKind::Binary { lhs, rhs, .. } if known.not_nan(place) => {
                self.checked += 1;
                let sides = (read(locals, lhs), read(locals, rhs));
                let nan = |value: &Value| matches!(value, Value::Float(number) if number.is_nan());
                (nan(&sides.0) || nan(&sides.1)).then(|| format!("{sides:?} has a NaN"))
            }
            _ => None,
        };
        match broken {
            Some(why) => Err(Stop::Broken(format!("in `{}` at {place:?}: {why}", function.name))),
            None => Ok(()),
        }
    }

    /// Checks what an instruction wrote to a local that isn't a list against what's known of it.
    fn check_written(&mut self, function: &Function, inst: &InstKind, locals: &[Value]) -> Ran<()> {
        let Some(known) = self.ranges_of(function) else { return Ok(()) };
        let Some(dest) = inst.dest() else { return Ok(()) };
        // A list's items are followed through the function, where it's written, not here.
        if function.locals.get(dest.0).is_some_and(|local| matches!(local.ty, fennec_check::Type::List(_))) {
            return Ok(());
        }
        let (Some(fact), Some(value)) = (known.written(dest), locals.get(dest.0)) else { return Ok(()) };
        self.checked += 1;
        if holds(value, fact) {
            return Ok(());
        }
        Err(Stop::Broken(format!(
            "in `{}`, _{} was written {value:?}, where {fact:?} says",
            function.name, dest.0
        )))
    }

    /// Checks what's known of a function's list parameters against the values they hold.
    fn check_params(&mut self, function: &Function, locals: &[Value], when: &str) -> Ran<()> {
        let Some(lengths) = self.lengths else { return Ok(()) };
        for at in 0..function.params {
            let Some(known) = lengths.param(&function.name, at) else { continue };
            self.checked += 1;
            let length = locals.get(at).and_then(length_of);
            let should = match known {
                Length::Fixed(number) => usize::try_from(number).ok(),
                Length::Same(other) => locals.get(other).and_then(length_of),
            };
            if length.is_none() || length != should {
                return Err(Stop::Broken(format!(
                    "{when} `{}`, parameter {at} is {length:?} long, where {known:?} says {should:?}",
                    function.name
                )));
            }
        }
        Ok(())
    }

    /// Checks what's known of the length of the list a function gives back.
    fn check_returned(&mut self, function: &Function, value: &Value, given: &[Option<usize>]) -> Ran<()> {
        let Some(known) = self.lengths.and_then(|lengths| lengths.returned(&function.name)) else {
            return Ok(());
        };
        self.checked += 1;
        let length = length_of(value);
        let should = match known {
            Length::Fixed(number) => usize::try_from(number).ok(),
            Length::Same(other) => given.get(other).copied().flatten(),
        };
        if length.is_none() || length != should {
            return Err(Stop::Broken(format!(
                "`{}` gave back a list {length:?} long, where {known:?} says {should:?}",
                function.name
            )));
        }
        Ok(())
    }

    /// Takes `count` steps from the budget.
    pub(crate) fn charge(&mut self, count: usize) -> Ran<()> {
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        if count > self.steps_left {
            self.steps_left = 0;
            return Err(Stop::TooLong);
        }
        self.steps_left -= count;
        Ok(())
    }

    /// A constant's value, worked out the first time it's read.
    fn constant(&mut self, name: &str, span: Span) -> Ran<Value> {
        match self.known.get(name) {
            Some(Known::Done(value)) => return Ok(value.clone()),
            Some(Known::Working) => return fail(format!("`{name}` depends on itself"), span),
            None => {}
        }
        let Some(function) = self.constants.get(name).copied() else {
            return not_here(format!("the constant `{name}`, which isn't lowered"));
        };
        self.known.insert(name.to_owned(), Known::Working);
        let worked = self.call(function, Vec::new()).map(|(value, _)| value);
        match &worked {
            Ok(value) => self.known.insert(name.to_owned(), Known::Done(value.clone())),
            // Another constant reading it tries again, within its own budget.
            Err(_) => self.known.remove(name),
        };
        worked
    }

    /// Calls a function, giving what it returns and its locals as they ended, for the `mut`
    /// parameters to be copied back from.
    pub(crate) fn call(&mut self, function: &'p Function, args: Vec<Value>) -> Ran<(Value, Vec<Value>)> {
        if self.depth >= DEEPEST {
            return not_here("calls this deep");
        }
        self.depth += 1;
        let ran = self.run(function, args);
        self.depth -= 1;
        ran
    }

    /// Calls a function by its key.
    pub(crate) fn call_named(&mut self, name: &str, args: Vec<Value>) -> Ran<(Value, Vec<Value>)> {
        let Some(function) = self.functions.get(name).copied() else {
            return not_here(format!("`{name}`, which isn't lowered here"));
        };
        self.call(function, args)
    }

    /// Calls a closure with these arguments after what it captured.
    pub(crate) fn call_closure(&mut self, closure: &Value, args: Vec<Value>) -> Ran<Value> {
        let Value::Closure(function, captured) = closure else {
            return not_here("calling something that isn't a closure");
        };
        let all = captured.iter().cloned().chain(args).collect();
        self.call_named(function, all).map(|(value, _)| value)
    }

    fn run(&mut self, function: &'p Function, args: Vec<Value>) -> Ran<(Value, Vec<Value>)> {
        let mut locals = args;
        self.check_given(function, &locals)?;
        let given: Vec<Option<usize>> = if self.lengths.is_some() {
            self.check_params(function, &locals, "on the way into")?;
            locals.iter().map(length_of).collect()
        } else {
            Vec::new()
        };
        locals.resize(function.locals.len(), Value::Unit);
        let mut at = 0;
        loop {
            let Some(block) = function.blocks.get(at) else {
                return not_here("a block that isn't there");
            };
            for (place, inst) in block.insts.iter().enumerate() {
                self.charge(1)?;
                self.check_operands(function, (at, place), &inst.kind, &locals)?;
                self.step(&mut locals, &inst.kind, inst.span)?;
                self.check_written(function, &inst.kind, &locals)?;
            }
            self.charge(1)?;
            at = match &block.end {
                Terminator::Jump(next) => next.0,
                Terminator::Branch { cond, then, otherwise } => match read(&locals, cond) {
                    Value::Bool(true) => then.0,
                    Value::Bool(false) => otherwise.0,
                    _ => return not_here("a branch on something that isn't a bool"),
                },
                Terminator::Return(value) => {
                    let value = value.as_ref().map_or(Value::Unit, |value| read(&locals, value));
                    if self.lengths.is_some() {
                        self.check_params(function, &locals, "on the way out of")?;
                        self.check_returned(function, &value, &given)?;
                    }
                    return Ok((value, locals));
                }
                Terminator::NoMatch { value, span } => {
                    let shown = text::repr(&read(&locals, value));
                    return match shown {
                        Some(shown) => fail(format!("no case matches {shown}"), *span),
                        None => not_here("a match no case takes"),
                    };
                }
                Terminator::Stop { message, span } => return fail(message.clone(), *span),
                Terminator::Unreachable => return not_here("code the checker says is unreachable"),
            };
        }
    }

    /// Runs one instruction.
    fn step(&mut self, locals: &mut [Value], inst: &'p InstKind, span: Span) -> Ran<()> {
        let value = match inst {
            // An optional, a `Some` and an error in a set of error types are the value itself here.
            InstKind::Copy { value, .. }
            | InstKind::MakeSome { value, .. }
            | InstKind::GetSome { value, .. }
            | InstKind::AsError { value, .. } => read(locals, value),
            InstKind::Unary { op, value, .. } => unary(*op, &read(locals, value), span)?,
            InstKind::Binary { op, lhs, rhs, .. } => {
                let (lhs, rhs) = (read(locals, lhs), read(locals, rhs));
                if let Some(size) = joined_size(*op, &lhs, &rhs) {
                    self.charge(size)?;
                }
                binary(*op, &lhs, &rhs, span)?
            }
            InstKind::Make { type_name, fields, .. } => {
                if self.program.structs.iter().any(|def| &def.name == type_name && def.shared) {
                    return not_here("a `shared` value");
                }
                instance(type_name, None, reads(locals, fields))
            }
            InstKind::GetField { object, field, .. } => {
                let object = read(locals, object);
                self.field(&object, field)?
            }
            InstKind::SetField { object, path, value } => {
                let value = read(locals, value);
                let fields = self.field_path(locals, *object, path)?;
                let place = slot(locals, *object)?;
                set_through(place, &fields, value)?;
                return Ok(());
            }
            InstKind::MakeCase { type_name, case, fields, .. } => {
                instance(type_name, Some(case), reads(locals, fields))
            }
            InstKind::IsCase { value, case, .. } => match read(locals, value) {
                Value::Instance(found) => Value::Bool(found.case.as_deref() == Some(case.as_str())),
                _ => return not_here("a case test of something that isn't an enum"),
            },
            InstKind::GetCaseField { value, case, field, .. } => {
                let value = read(locals, value);
                let Value::Instance(found) = &value else {
                    return not_here("a case's field of something that isn't an enum");
                };
                let key = format!("{}.{case}", found.type_name);
                let at = self.place_of(&key, field)?;
                found.fields.get(at).cloned().ok_or(Stop::NotHere("a missing field".to_owned()))?
            }
            InstKind::MakeList { items, .. } | InstKind::ConstantList { items, .. } => {
                Value::list(reads(locals, items))
            }
            InstKind::MakeSet { items, .. } => Value::set(reads(locals, items)),
            InstKind::MakeDict { pairs, .. } => Value::Dict(Rc::new(
                pairs.iter().map(|(key, value)| (read(locals, key), read(locals, value))).collect(),
            )),
            InstKind::MakeTuple { items, .. } => Value::Tuple(Rc::new(reads(locals, items))),
            InstKind::GetTupleItem { value, at, .. } => match read(locals, value) {
                Value::Tuple(items) => {
                    items.get(*at).cloned().ok_or(Stop::NotHere("a missing item".to_owned()))?
                }
                _ => return not_here("a tuple's item of something that isn't a tuple"),
            },
            InstKind::GetIndex { object, index, .. } => {
                let (object, index) = (read(locals, object), read(locals, index));
                item_at(&object, &index, span)?
            }
            InstKind::Slice { object, start, end, .. } => {
                let object = read(locals, object);
                let start = start.as_ref().map(|start| read(locals, start));
                let end = end.as_ref().map(|end| read(locals, end));
                slice(&object, start.as_ref(), end.as_ref())?
            }
            InstKind::SetIndex { object, path, index, value } => {
                let (index, value) = (read(locals, index), read(locals, value));
                let fields = self.field_path(locals, *object, path)?;
                let place = slot(locals, *object)?;
                let held = reach(place, &fields)?;
                return set_item(held, index, value, span);
            }
            InstKind::Format { parts, .. } => {
                let mut written = String::new();
                for part in parts {
                    match part {
                        crate::FormatPart::Text(piece) => written.push_str(piece),
                        crate::FormatPart::Value { value, spec } => {
                            let value = read(locals, value);
                            let Some(shown) = text::formatted(&value, spec.as_deref()) else {
                                return not_here("formatting this value");
                            };
                            written.push_str(&shown);
                        }
                    }
                }
                self.charge(written.len() / 16)?;
                Value::Str(Rc::from(written))
            }
            InstKind::Builtin { dest, name, .. }
                if self.quiet && (name.starts_with("Stdout.") || name.starts_with("Stderr.")) =>
            {
                if let Some(dest) = dest {
                    *slot(locals, *dest)? = Value::Unit;
                }
                return Ok(());
            }
            InstKind::Builtin { dest, name, args, changes } => {
                let mut values = reads(locals, args);
                let changed_local = match args.first() {
                    Some(Operand::Local(first)) if *changes => Some(*first),
                    _ => None,
                };
                // What a builtin changes is taken from its local rather than copied beside it, so
                // appending to a list doesn't copy it whole. It's put back if it isn't changed.
                if let (Some(first), Some(taken)) = (changed_local, values.first_mut()) {
                    *taken = std::mem::replace(slot(locals, first)?, Value::Unit);
                }
                let (result, changed) = builtins::call(self, name, &mut values, span)?;
                if let Some(first) = changed_local {
                    let back = match changed {
                        Some(changed) => changed,
                        None => values.into_iter().next().unwrap_or(Value::Unit),
                    };
                    *slot(locals, first)? = back;
                }
                if let Some(dest) = dest {
                    *slot(locals, *dest)? = result;
                }
                return Ok(());
            }
            InstKind::IsNone { value, .. } => Value::Bool(matches!(read(locals, value), Value::None)),
            InstKind::MakeMember { value, member, .. } => {
                Value::Member(*member, Rc::new(read(locals, value)))
            }
            InstKind::IsMember { value, member, .. } => match read(locals, value) {
                Value::Member(held, _) => Value::Bool(held == *member),
                _ => return not_here("a union test of something that isn't a union"),
            },
            InstKind::GetMember { value, .. } => match read(locals, value) {
                Value::Member(_, inner) => inner.as_ref().clone(),
                _ => return not_here("a union's member of something that isn't a union"),
            },
            InstKind::MakeOk { value, .. } => {
                Value::Ok(Rc::new(value.as_ref().map_or(Value::Unit, |value| read(locals, value))))
            }
            InstKind::MakeErr { error, .. } => Value::Err(Rc::new(read(locals, error))),
            InstKind::IsErr { value, .. } => match read(locals, value) {
                Value::Ok(_) => Value::Bool(false),
                Value::Err(_) => Value::Bool(true),
                _ => return not_here("a result test of something that isn't a result"),
            },
            InstKind::GetOk { value, .. } | InstKind::GetErr { value, .. } => match read(locals, value) {
                Value::Ok(inner) | Value::Err(inner) => inner.as_ref().clone(),
                _ => return not_here("what's in something that isn't a result"),
            },
            InstKind::IsError { value, type_name, .. } => match read(locals, value) {
                Value::Instance(found) => Value::Bool(found.type_name.as_ref() == type_name),
                _ => return not_here("an error test of something that isn't an error"),
            },
            InstKind::MakeClosure { function, captures, .. } => {
                Value::Closure(Rc::from(function.as_str()), Rc::new(reads(locals, captures)))
            }
            InstKind::CallClosure { dest, closure, args } => {
                let (closure, args) = (read(locals, closure), reads(locals, args));
                let result = self.call_closure(&closure, args)?;
                if let Some(dest) = dest {
                    *slot(locals, *dest)? = result;
                }
                return Ok(());
            }
            InstKind::Constant { name, .. } => self.constant(name, span)?,
            InstKind::Call { dest, function, args } => {
                let Some(callee) = self.functions.get(function.as_str()).copied() else {
                    return not_here(format!("`{function}`, which isn't lowered here"));
                };
                let (result, ended) = self.call(callee, reads(locals, args))?;
                for &param in &callee.mut_params {
                    if let (Some(Operand::Local(back)), Some(value)) = (args.get(param), ended.get(param)) {
                        *slot(locals, *back)? = value.clone();
                    }
                }
                if let Some(dest) = dest {
                    *slot(locals, *dest)? = result;
                }
                return Ok(());
            }
            InstKind::MakeGenerator { .. }
            | InstKind::GetSlot { .. }
            | InstKind::SetSlot { .. }
            | InstKind::Resume { .. } => {
                return not_here("a generator");
            }
            InstKind::Lock { .. }
            | InstKind::Unlock { .. }
            | InstKind::CrashCleanup { .. }
            | InstKind::CrashCleanupDone { .. }
            | InstKind::LockAll { .. }
            | InstKind::UnlockAll { .. } => {
                return not_here("a `shared` value");
            }
        };
        if let Some(dest) = inst.dest() {
            *slot(locals, dest)? = value;
        }
        Ok(())
    }

    /// Where a field is among its type's, by `Type` or `Type.Case`.
    fn place_of(&self, key: &str, field: &str) -> Ran<usize> {
        let names = self.fields.get(key).ok_or(Stop::NotHere(format!("the fields of `{key}`")))?;
        names.iter().position(|name| *name == field).ok_or(Stop::NotHere(format!("`{key}.{field}`")))
    }

    /// A struct's or error's field.
    pub(crate) fn field(&self, object: &Value, field: &str) -> Ran<Value> {
        let Value::Instance(found) = object else {
            return not_here(format!("`.{field}` of something that isn't a struct"));
        };
        let at = self.place_of(&found.type_name, field)?;
        found.fields.get(at).cloned().ok_or(Stop::NotHere("a missing field".to_owned()))
    }

    /// The places of each field along `path` from a local's struct.
    fn field_path(&self, locals: &[Value], object: LocalId, path: &[String]) -> Ran<Vec<usize>> {
        let mut held = locals.get(object.0).cloned().unwrap_or(Value::Unit);
        let mut places = Vec::new();
        for field in path {
            let Value::Instance(found) = &held else {
                return not_here(format!("`.{field}` of something that isn't a struct"));
            };
            let at = self.place_of(&found.type_name, field)?;
            places.push(at);
            held = found.fields.get(at).cloned().unwrap_or(Value::Unit);
        }
        Ok(places)
    }
}

/// `object[index]`.
fn item_at(object: &Value, index: &Value, span: Span) -> Ran<Value> {
    if let Value::Dict(pairs) = object {
        return match pairs.get(index) {
            Some(value) => Ok(value.clone()),
            None => match text::repr(index) {
                Some(shown) => fail(format!("the dict has no key {shown}"), span),
                None => not_here("a key a dict doesn't have"),
            },
        };
    }
    let Value::Int(at) = index else {
        return not_here("an index that isn't an int");
    };
    let Some(len) = object.count() else {
        return not_here("indexing this value");
    };
    let place = position(*at, len, span)?;
    let found = match object {
        Value::List(items) | Value::Tuple(items) => items.get(place).cloned(),
        Value::Str(text) => text.chars().nth(place).map(|c| Value::str(c.encode_utf8(&mut [0; 4]))),
        Value::Bytes(data) => data.get(place).map(|byte| Value::Int(i64::from(*byte))),
        _ => None,
    };
    found.ok_or(Stop::NotHere("indexing this value".to_owned()))
}

/// The value an operand reads.
fn read(locals: &[Value], operand: &Operand) -> Value {
    match operand {
        Operand::Local(id) => locals.get(id.0).cloned().unwrap_or(Value::Unit),
        Operand::Int(value) => Value::Int(*value),
        Operand::Float(value) => Value::Float(*value),
        Operand::Bool(value) => Value::Bool(*value),
        Operand::Str(text) => Value::str(text),
        Operand::Bytes(blob) => Value::Bytes(Rc::from(blob.0.as_ref())),
        Operand::None => Value::None,
    }
}

fn reads(locals: &[Value], operands: &[Operand]) -> Vec<Value> {
    operands.iter().map(|operand| read(locals, operand)).collect()
}

fn slot(locals: &mut [Value], local: LocalId) -> Ran<&mut Value> {
    locals.get_mut(local.0).ok_or(Stop::NotHere("a local that isn't there".to_owned()))
}

fn instance(type_name: &str, case: Option<&String>, fields: Vec<Value>) -> Value {
    Value::Instance(Rc::new(value::Instance {
        type_name: Rc::from(type_name),
        case: case.map(|case| Rc::from(case.as_str())),
        fields,
    }))
}

/// The value at the end of a path of fields, to change.
fn reach<'v>(mut held: &'v mut Value, fields: &[usize]) -> Ran<&'v mut Value> {
    for &at in fields {
        let Value::Instance(found) = held else {
            return not_here("a field of something that isn't a struct");
        };
        held = Rc::make_mut(found).fields.get_mut(at).ok_or(Stop::NotHere("a missing field".to_owned()))?;
    }
    Ok(held)
}

/// Sets the field at the end of a path.
fn set_through(held: &mut Value, fields: &[usize], value: Value) -> Ran<()> {
    *reach(held, fields)? = value;
    Ok(())
}

/// Sets a list's item or a dict's value for a key.
fn set_item(held: &mut Value, index: Value, value: Value, span: Span) -> Ran<()> {
    match (held, index) {
        (Value::List(items), Value::Int(at)) => {
            let place = position(at, items.len(), span)?;
            if let Some(item) = Rc::make_mut(items).get_mut(place) {
                *item = value;
            }
            Ok(())
        }
        (Value::Dict(pairs), key) => {
            Rc::make_mut(pairs).insert(key, value);
            Ok(())
        }
        _ => not_here("setting an item of this value"),
    }
}

/// A place from a possibly negative index, as the engines resolve it.
pub(crate) fn position(at: i64, len: usize, span: Span) -> Ran<usize> {
    let length = i64::try_from(len).unwrap_or(i64::MAX);
    let resolved = if at < 0 { at + length } else { at };
    match usize::try_from(resolved) {
        Ok(place) if place < len => Ok(place),
        _ => fail(format!("index {at} is out of range for length {len}"), span),
    }
}

/// `value[start:end]`: positions count from the back when negative, and stop at either end.
fn slice(value: &Value, start: Option<&Value>, end: Option<&Value>) -> Ran<Value> {
    let Some(len) = value.count().filter(|_| !matches!(value, Value::Set(_) | Value::Dict(_))) else {
        return not_here("slicing this value");
    };
    let length = i64::try_from(len).unwrap_or(i64::MAX);
    let bound = |at: Option<&Value>, default: usize| -> Ran<usize> {
        match at {
            None => Ok(default),
            Some(Value::Int(at)) => {
                let at = if *at < 0 { at.saturating_add(length) } else { *at };
                Ok(usize::try_from(at.clamp(0, length)).unwrap_or(0))
            }
            Some(_) => not_here("a slice position that isn't an int"),
        }
    };
    let from = bound(start, 0)?;
    let to = bound(end, len)?.max(from);
    Ok(match value {
        Value::List(items) => Value::list(items.get(from..to).unwrap_or_default().to_vec()),
        Value::Tuple(items) => Value::Tuple(Rc::new(items.get(from..to).unwrap_or_default().to_vec())),
        Value::Str(text) => Value::str(&text.chars().skip(from).take(to - from).collect::<String>()),
        Value::Bytes(data) => Value::Bytes(Rc::from(data.get(from..to).unwrap_or_default())),
        _ => return not_here("slicing this value"),
    })
}

/// How many items or bytes `+` makes, to charge for, when it joins strs, bytes or lists.
fn joined_size(op: BinOp, lhs: &Value, rhs: &Value) -> Option<usize> {
    if op != BinOp::Add {
        return None;
    }
    let size = |value: &Value| match value {
        Value::Str(text) => Some(text.len() / 16),
        Value::Bytes(data) => Some(data.len() / 16),
        Value::List(items) => Some(items.len()),
        _ => None,
    };
    Some(size(lhs)? + size(rhs)?)
}

fn unary(op: UnaryOp, value: &Value, span: Span) -> Ran<Value> {
    match (op, value) {
        (UnaryOp::Neg, Value::Int(n)) => n.checked_neg().map(Value::Int).ok_or_else(|| overflow(span)),
        (UnaryOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
        (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        (UnaryOp::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
        _ => not_here("this operator"),
    }
}

fn overflow(span: Span) -> Stop {
    Stop::Failed(
        Diagnostic::new("this calculation overflows an int", span)
            .with_help("ints are 64-bit; check the values involved, or use floats for very large numbers"),
    )
}

fn binary(op: BinOp, lhs: &Value, rhs: &Value, span: Span) -> Ran<Value> {
    match op {
        BinOp::Eq => return Ok(Value::Bool(lhs == rhs)),
        BinOp::NotEq => return Ok(Value::Bool(lhs != rhs)),
        BinOp::Is => return Ok(Value::Bool(matches!(lhs, Value::None) == matches!(rhs, Value::None))),
        BinOp::IsNot => return Ok(Value::Bool(matches!(lhs, Value::None) != matches!(rhs, Value::None))),
        BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            let ordering = builtins::order(lhs, rhs)?;
            return Ok(Value::Bool(match op {
                BinOp::Lt => ordering.is_lt(),
                BinOp::LtEq => ordering.is_le(),
                BinOp::Gt => ordering.is_gt(),
                _ => ordering.is_ge(),
            }));
        }
        BinOp::In | BinOp::NotIn => {
            let found = contains(rhs, lhs)?;
            return Ok(Value::Bool(found == (op == BinOp::In)));
        }
        _ => {}
    }
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => int_arithmetic(op, *a, *b, span),
        (Value::Float(a), Value::Float(b)) => float_arithmetic(op, *a, *b, span),
        (Value::Str(a), Value::Str(b)) if op == BinOp::Add => Ok(Value::Str(Rc::from(format!("{a}{b}")))),
        (Value::Bytes(a), Value::Bytes(b)) if op == BinOp::Add => {
            Ok(Value::Bytes(a.iter().chain(b.iter()).copied().collect()))
        }
        (Value::List(a), Value::List(b)) if op == BinOp::Add => {
            Ok(Value::list(a.iter().chain(b.iter()).cloned().collect()))
        }
        (Value::Bool(a), Value::Bool(b)) => match op {
            BinOp::BitAnd => Ok(Value::Bool(a & b)),
            BinOp::BitOr => Ok(Value::Bool(a | b)),
            BinOp::BitXor => Ok(Value::Bool(a ^ b)),
            _ => not_here("this operator"),
        },
        _ => not_here("this operator"),
    }
}

/// Whether a container holds an item, or a str a part.
fn contains(container: &Value, item: &Value) -> Ran<bool> {
    match (container, item) {
        (Value::List(items) | Value::Tuple(items), item) => Ok(items.contains(item)),
        (Value::Set(items) | Value::Dict(items), key) => Ok(items.contains(key)),
        (Value::Str(text), Value::Str(part)) => Ok(text.contains(part.as_ref())),
        (Value::Bytes(data), Value::Int(byte)) => {
            Ok(u8::try_from(*byte).is_ok_and(|byte| data.contains(&byte)))
        }
        _ => not_here("`in` on this value"),
    }
}

fn int_arithmetic(op: BinOp, a: i64, b: i64, span: Span) -> Ran<Value> {
    if b == 0 && matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
        return fail("division by zero", span);
    }
    let result = match op {
        BinOp::BitAnd => return Ok(Value::Int(a & b)),
        BinOp::BitOr => return Ok(Value::Int(a | b)),
        BinOp::BitXor => return Ok(Value::Int(a ^ b)),
        BinOp::ShiftLeft | BinOp::ShiftRight => {
            let Some(count) = u32::try_from(b).ok().filter(|count| *count < 64) else {
                return Err(Stop::Failed(
                    Diagnostic::new("can't shift by a count outside 0 to 63", span)
                        .with_help("ints have 64 bits, so check the count first"),
                ));
            };
            let shifted = if op == BinOp::ShiftLeft { a.wrapping_shl(count) } else { a.wrapping_shr(count) };
            return Ok(Value::Int(shifted));
        }
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        BinOp::Mul => a.checked_mul(b),
        BinOp::Div => return Ok(Value::Float(builtins::to_float(a) / builtins::to_float(b))),
        BinOp::FloorDiv => {
            a.checked_div_euclid(b).map(|q| if b.is_negative() && a.rem_euclid(b) != 0 { q - 1 } else { q })
        }
        BinOp::Mod => a.checked_rem_euclid(b).map(|r| if b.is_negative() && r != 0 { r + b } else { r }),
        BinOp::Pow => {
            let Ok(exponent) = u32::try_from(b) else {
                return Err(Stop::Failed(
                    Diagnostic::new("an int can only be raised to a non-negative power", span)
                        .with_help("for fractional results, use floats: float(a) ** float(b)"),
                ));
            };
            a.checked_pow(exponent)
        }
        _ => return not_here("this operator"),
    };
    result.map(Value::Int).ok_or_else(|| overflow(span))
}

fn float_arithmetic(op: BinOp, a: f64, b: f64, span: Span) -> Ran<Value> {
    if b == 0.0 && matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
        return fail("division by zero", span);
    }
    Ok(Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::FloorDiv => (a / b).floor(),
        BinOp::Mod => a - b * (a / b).floor(),
        BinOp::Pow => builtins::float_power(a, b),
        _ => return not_here("this operator"),
    }))
}
