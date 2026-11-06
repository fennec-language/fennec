//! The checks a program's native code keeps: each place that can still stop the program with an
//! overflow, an index outside, a division by zero and the like, because the analysis emit uses
//! couldn't prove it never does. Worked out from the IR and that analysis alone, with no LLVM.
//!
//! LLVM drops a check on a constant it can see the answer to, as `x // 2` or `x << 3`, and
//! those aren't here. It can drop others too, from what it learns while optimizing; those are
//! still here, since nothing promises it will.

use fennec_check::Type;
use fennec_ir::{Function, InstKind, LocalId, Operand, PlaceStep, Program};
use fennec_syntax::{BinOp, Span, UnaryOp};

use crate::analysis::{Analysis, builtin_fails, visit, whole};

/// A check the native code keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckLeft {
    /// The function it's in, by its name in the IR.
    pub function: String,
    pub span: Span,
    pub what: CheckKind,
}

/// What a check is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckKind {
    /// An int `+`, `-`, `*`, `**` or negation that might overflow, or `//` or `%` by what might be
    /// -1 with the lowest int.
    Overflow,
    /// An index that might be outside its list, array or string.
    Bounds,
    /// A key that might not be in its dict, or an `Id` whose item might be gone.
    MissingKey,
    /// A `/`, `//` or `%` by what might be zero.
    DivideByZero,
    /// A shift by what might be outside 0 to 63.
    ShiftRange,
    /// A builtin that can fail where it runs, by its name.
    Builtin(String),
}

/// Every check the native code keeps, function by function in the program's order, each in the
/// order of its blocks.
#[must_use]
pub fn checks_left(program: &Program) -> Vec<CheckLeft> {
    let everything = whole(program);
    let mut found = Vec::new();
    for function in program.functions.iter().chain(&program.constants) {
        let Some(known) = everything.analyses.get(function.name.as_str()) else { continue };
        let mut add = |span: Span, what: CheckKind| {
            found.push(CheckLeft { function: function.name.clone(), span, what });
        };
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            for what in checks_in(program, function, known, &inst.kind) {
                add(inst.span, what);
            }
        }
        for (span, what) in place_checks(function, known) {
            add(span, what);
        }
    }
    found
}

/// The checks one instruction keeps.
fn checks_in(program: &Program, function: &Function, known: &Analysis, inst: &InstKind) -> Vec<CheckKind> {
    match inst {
        InstKind::Binary { dest, op, lhs, rhs } => match (function.type_of(lhs), op) {
            (Some(Type::Int), BinOp::Add | BinOp::Sub | BinOp::Mul) if !known.is_safe(*dest) => {
                vec![CheckKind::Overflow]
            }
            (Some(Type::Int), BinOp::Pow) => vec![CheckKind::Overflow],
            (Some(Type::Int), BinOp::FloorDiv | BinOp::Mod) => match rhs {
                Operand::Int(0) => vec![CheckKind::DivideByZero],
                Operand::Int(-1) => vec![CheckKind::Overflow],
                Operand::Int(_) => Vec::new(),
                _ => vec![CheckKind::DivideByZero, CheckKind::Overflow],
            },
            (Some(Type::Int), BinOp::Div) => match rhs {
                Operand::Int(0) => vec![CheckKind::DivideByZero],
                Operand::Int(_) => Vec::new(),
                _ => vec![CheckKind::DivideByZero],
            },
            (Some(Type::Int), BinOp::ShiftLeft | BinOp::ShiftRight) => match rhs {
                Operand::Int(0..64) => Vec::new(),
                _ => vec![CheckKind::ShiftRange],
            },
            (Some(Type::Float), BinOp::Div | BinOp::FloorDiv | BinOp::Mod) if !known.is_nonzero(rhs) => {
                vec![CheckKind::DivideByZero]
            }
            _ => Vec::new(),
        },
        InstKind::Unary { dest, op: UnaryOp::Neg, value }
            if function.type_of(value) == Some(Type::Int) && !known.is_safe(*dest) =>
        {
            vec![CheckKind::Overflow]
        }
        InstKind::Builtin { name, args, .. } if builtin_fails(function, name, args) => {
            vec![CheckKind::Builtin(name.clone())]
        }
        InstKind::GetIndex { object, index, .. } => {
            let inside = matches!(object, Operand::Local(list) if known.inside(*list, index));
            index_check(function.type_of(object).as_ref(), index, inside)
        }
        InstKind::SetIndex { object, path, index, .. } => {
            let ty = function.type_of(&Operand::Local(*object)).and_then(|ty| along(program, ty, path));
            let inside = path.is_empty() && known.inside(*object, index);
            match ty {
                // Setting a dict's key adds it if it's missing.
                Some(Type::Dict(..)) => Vec::new(),
                ty => index_check(ty.as_ref(), index, inside),
            }
        }
        _ => Vec::new(),
    }
}

/// What reading `object[index]` of this type checks.
fn index_check(ty: Option<&Type>, index: &Operand, inside: bool) -> Vec<CheckKind> {
    match ty {
        Some(Type::Dict(..) | Type::Table(_)) => vec![CheckKind::MissingKey],
        // A tuple's index is always written out, and checked by the checker.
        Some(Type::Tuple(_)) => Vec::new(),
        Some(Type::Array(_, count)) => {
            let count = i64::try_from(*count).unwrap_or(i64::MAX);
            match index {
                Operand::Int(at) if (-count..count).contains(at) => Vec::new(),
                _ => vec![CheckKind::Bounds],
            }
        }
        _ if inside => Vec::new(),
        _ => vec![CheckKind::Bounds],
    }
}

/// The type of what's at a field path from a value of type `ty`.
fn along(program: &Program, ty: Type, path: &[String]) -> Option<Type> {
    path.iter().try_fold(ty, |ty, name| {
        let Type::Named(owner) = ty else { return None };
        let def = program.structs.iter().find(|def| def.name == owner)?;
        def.fields.iter().find(|(field, _)| field == name).map(|(_, field)| field.clone())
    })
}

/// The index checks on the way to each place a call changes, taken out for it, as `walk_place`
/// makes them: an index before any field, known to be inside the root's list, has none. Each is
/// placed where the place is first used.
fn place_checks(function: &Function, known: &Analysis) -> Vec<(Span, CheckKind)> {
    let mut found = Vec::new();
    for place in &function.places {
        if known.place_taken(function, place.value).is_none() {
            continue;
        }
        let mut fields = 0;
        let mut checks = 0;
        for step in &place.steps {
            match step {
                PlaceStep::Field(_) => fields += 1,
                PlaceStep::Item(index) => {
                    checks += usize::from(fields > 0 || !known.inside(place.root, index));
                }
            }
        }
        let span = first_use(function, place.value);
        found.extend(std::iter::repeat_n((span, CheckKind::Bounds), checks));
    }
    found
}

/// Where a local is first read or written.
fn first_use(function: &Function, local: LocalId) -> Span {
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        let used = std::cell::Cell::new(false);
        visit(
            &inst.kind,
            |read| used.set(used.get() || *read == Operand::Local(local)),
            |write| used.set(used.get() || write == local),
        );
        if used.get() {
            return inst.span;
        }
    }
    Span::default()
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::{CheckKind, checks_left};

    fn lowered(body: &str) -> Result<fennec_ir::Program, String> {
        let module = fennec_syntax::parse(body).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        Ok(fennec_ir::lower(&module, &checked))
    }

    /// The checks `go` keeps, by kind, with the source each is on.
    fn kept(body: &str) -> Result<Vec<(CheckKind, String)>, String> {
        let program = lowered(body)?;
        let on = |start: u32, end: u32| body.get(start as usize..end as usize).unwrap_or_default().to_owned();
        Ok(checks_left(&program)
            .into_iter()
            .filter(|check| check.function == "go")
            .map(|check| (check.what, on(check.span.start, check.span.end)))
            .collect())
    }

    /// The overflow checks emit writes for `+`, `-` and `*` are the ones reported, for programs with
    /// no other kind of overflow check.
    #[test]
    fn the_overflow_checks_reported_are_the_ones_emitted() -> Result<(), String> {
        for body in [
            "def go(xs: list[int]) -> int:\n    mut total = 0\n    for i in range(xs.len()):\n        total += xs[i] * 3 - i\n    total\n",
            "def go(n: int) -> int:\n    mut total = 0\n    for i in range(n):\n        total += i * i\n    total + n - 1\n",
            "def go(a: int, b: int) -> int:\n    if a > 0 and a < 100 and b > 0 and b < 100:\n        return a * b + a - b\n    a + b\n",
        ] {
            let program = lowered(body)?;
            let reported = checks_left(&program)
                .iter()
                .filter(|check| check.function == "go" && check.what == CheckKind::Overflow)
                .count();
            let (units, _, _) = crate::emit(&program)?;
            let text = &units.iter().find(|unit| unit.function == "go").ok_or("`go` isn't emitted")?.text;
            let emitted = ["sadd", "ssub", "smul"]
                .iter()
                .map(|op| text.matches(&format!("call {{ i64, i1 }} @llvm.{op}.with.overflow.i64(")).count())
                .sum::<usize>();
            assert_eq!(reported, emitted, "{body}\n{text}");
        }
        Ok(())
    }

    #[test]
    fn a_loop_over_a_list_s_positions_keeps_only_its_sum_s_check() -> Result<(), String> {
        let found = kept(
            "def go(xs: list[int]) -> int:\n    mut total = 0\n    for i in range(xs.len()):\n        total += xs[i]\n    total\n",
        )?;
        assert_eq!(found, [(CheckKind::Overflow, "total += xs[i]".to_owned())]);
        Ok(())
    }

    #[test]
    fn divisions_and_shifts_keep_what_a_constant_doesn_t_settle() -> Result<(), String> {
        let found = kept(
            "def go(a: int, b: int, f: float) -> float:\n    c = a // 2 + a % 7 + (a << 3)\n    d = a // b\n    e = a << b\n    float(c + d + e) / f + f / 2.0\n",
        )?;
        let has = |what: CheckKind, on: &str| found.contains(&(what, on.to_owned()));
        assert!(has(CheckKind::DivideByZero, "a // b"), "{found:?}");
        assert!(has(CheckKind::Overflow, "a // b"), "{found:?}");
        assert!(has(CheckKind::ShiftRange, "a << b"), "{found:?}");
        assert!(has(CheckKind::DivideByZero, "float(c + d + e) / f"), "{found:?}");
        let settled = ["a // 2", "a % 7", "a << 3", "f / 2.0"];
        assert!(!found.iter().any(|(_, on)| settled.contains(&on.as_str())), "{found:?}");
        assert_eq!(found.iter().filter(|(what, _)| *what == CheckKind::ShiftRange).count(), 1, "{found:?}");
        Ok(())
    }

    #[test]
    fn an_index_from_anywhere_and_a_key_keep_their_checks() -> Result<(), String> {
        let found = kept(
            "def go(xs: list[int], at: int, names: dict[str, int], fixed: array[int, 4]) -> int:\n    xs[at] + names[\"a\"] + fixed[2] + fixed[at]\n",
        )?;
        let has = |what: CheckKind, on: &str| found.contains(&(what, on.to_owned()));
        assert!(has(CheckKind::Bounds, "xs[at]"), "{found:?}");
        assert!(has(CheckKind::MissingKey, "names[\"a\"]"), "{found:?}");
        assert!(has(CheckKind::Bounds, "fixed[at]"), "{found:?}");
        assert!(!found.iter().any(|(_, on)| on == "fixed[2]"), "{found:?}");
        Ok(())
    }
}
