//! Lowering checked programs to the IR: the printed form is pinned for a few functions, and
//! everything lowered from every example has to pass the validator.

use std::fs;
use std::path::Path;

use fennec_check::Type;
use fennec_ir::{Block, Function, Local, LocalId, Operand, Program, Terminator, lower, validate};

fn lowered(source: &str) -> Result<Program, String> {
    let module = fennec_syntax::parse(source).map_err(|problem| problem.message)?;
    let checked = fennec_check::check_typed(&module).map_err(|problems| format!("{problems:?}"))?;
    let program = lower(&module, &checked);
    validate(&program).map_err(|problems| format!("{problems:?}"))?;
    Ok(program)
}

/// The printed form of one lowered function.
fn printed(source: &str, name: &str) -> Result<String, String> {
    let program = lowered(source)?;
    let function = program.functions.iter().find(|function| function.name == name);
    function
        .map(ToString::to_string)
        .ok_or_else(|| format!("{name} wasn't lowered: {:?}", program.not_lowered))
}

#[test]
fn operators_and_tail_values() {
    let source = "def lerp(a: float, b: float, t: float) -> float:\n    a + (b - a) * t\n";
    assert_eq!(
        printed(source, "lerp").unwrap(),
        "fn lerp(_0, _1, _2) -> float
  local _0 a: float
  local _1 b: float
  local _2 t: float
  local _3: float
  local _4: float
  local _5: float
  bb0:
    _3 = _1 - _0
    _4 = _3 * _2
    _5 = _0 + _4
    return _5
"
    );
}

#[test]
fn recursion_and_early_return() {
    let source = "def fib(n: int) -> int:\n    if n < 2: return n\n    return fib(n - 1) + fib(n - 2)\n";
    assert_eq!(
        printed(source, "fib").unwrap(),
        "fn fib(_0) -> int
  local _0 n: int
  local _1: bool
  local _2: int
  local _3: int
  local _4: int
  local _5: int
  local _6: int
  bb0:
    _1 = _0 < 2
    branch _1 bb2 bb3
  bb1:
    _2 = _0 - 1
    _3 = call fib(_2)
    _4 = _0 - 2
    _5 = call fib(_4)
    _6 = _3 + _5
    return _6
  bb2:
    return _0
  bb3:
    jump bb1
"
    );
}

#[test]
fn loops_and_changing_names() {
    let source = "def count(limit: int) -> int:
    mut n = 0
    while True:
        n += 1
        if n == limit: break
    n
";
    assert_eq!(
        printed(source, "count").unwrap(),
        "fn count(_0) -> int
  local _0 limit: int
  local _1 n: int
  local _2: bool
  bb0:
    _1 = 0
    jump bb1
  bb1:
    jump bb2
  bb2:
    _1 = _1 + 1
    _2 = _1 == _0
    branch _2 bb5 bb6
  bb3:
    return _1
  bb4:
    jump bb1
  bb5:
    jump bb3
  bb6:
    jump bb4
"
    );
}

#[test]
fn and_or_and_conditional_values_branch() {
    let source = "def pick(a: bool, b: bool) -> int:\n    1 if a and b else 2\n";
    assert_eq!(
        printed(source, "pick").unwrap(),
        "fn pick(_0, _1) -> int
  local _0 a: bool
  local _1 b: bool
  local _2: int
  local _3: bool
  bb0:
    _3 = _0
    branch _3 bb1 bb2
  bb1:
    _3 = _1
    jump bb2
  bb2:
    branch _3 bb3 bb4
  bb3:
    _2 = 1
    jump bb5
  bb4:
    _2 = 2
    jump bb5
  bb5:
    return _2
"
    );
}

#[test]
fn names_in_sibling_blocks_are_different_locals() {
    let source = "def f(a: bool) -> int:
    if a:
        x = 1
        return x
    else:
        x = \"one\"
        return 2
";
    let function = printed(source, "f").unwrap();
    assert!(function.contains("local _1 x: int"), "{function}");
    assert!(function.contains("local _2 x: str"), "{function}");
}

#[test]
fn what_isnt_lowered_yet_says_why() {
    let source = "struct Point:
    x: int

def main(stdout: Stdout):
    stdout.print(1)

def first(items: list[int]) -> int:
    items[0]

def make() -> Point:
    Point(1)

def size(n: int) -> int:
    total = 0
    for i in range(n):
        pass
    total

def calls_first() -> int:
    first([1])
";
    let program = lowered(source).unwrap();
    let why: Vec<String> =
        program.not_lowered.iter().map(|skipped| format!("{}: {}", skipped.function, skipped.why)).collect();
    assert_eq!(
        why,
        [
            "main: `stdout` is Stdout",
            "first: `items` is list[int]",
            "make: it gives back Point",
            "size: it has a `for` loop",
            "calls_first: `first` takes something other than plain values",
        ]
    );
}

/// A one-block function with the given locals, ending as given.
fn function(locals: Vec<Type>, end: Terminator) -> Function {
    Function {
        name: "f".to_owned(),
        params: 0,
        returns: Type::Int,
        locals: locals.into_iter().map(|ty| Local { name: None, ty }).collect(),
        blocks: vec![Block { insts: Vec::new(), end }],
    }
}

#[test]
fn the_validator_finds_malformed_ir() {
    let problems = |function: Function| {
        validate(&Program { functions: vec![function], not_lowered: Vec::new() }).unwrap_err()
    };
    assert_eq!(
        problems(function(Vec::new(), Terminator::Return(Some(Operand::Local(LocalId(0)))))),
        ["f: uses _0, which doesn't exist"]
    );
    assert_eq!(
        problems(function(Vec::new(), Terminator::Return(Some(Operand::Bool(true))))),
        ["f: return `true` has the wrong type"]
    );
    assert_eq!(
        problems(function(Vec::new(), Terminator::Jump(fennec_ir::BlockId(3)))),
        ["f: goes to bb3, which doesn't exist"]
    );
    assert_eq!(
        problems(function(
            Vec::new(),
            Terminator::Branch {
                cond: Operand::Int(1),
                then: fennec_ir::BlockId(0),
                otherwise: fennec_ir::BlockId(0)
            }
        )),
        ["f: condition `1` has the wrong type"]
    );
    assert_eq!(
        problems(function(Vec::new(), Terminator::Return(None))),
        ["f: returns nothing, but has a return type"]
    );
}

#[test]
fn every_example_lowers_to_valid_ir_or_says_why() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut lowered_functions = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "fen") {
            continue;
        }
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = fs::read_to_string(&path).unwrap();
        let module = fennec_syntax::parse(&source).unwrap();
        let checked =
            fennec_check::check_typed(&module).unwrap_or_else(|problems| panic!("{file}: {problems:?}"));
        let program = lower(&module, &checked);
        validate(&program).unwrap_or_else(|problems| panic!("{file}: {problems:?}\n{program}"));
        lowered_functions
            .extend(program.functions.iter().map(|function| format!("{file}: {}", function.name)));
    }
    for expected in
        ["numbers.fen: fib", "numbers.fen: collatz_steps", "numbers.fen: is_prime", "numbers.fen: lerp"]
    {
        assert!(lowered_functions.iter().any(|found| found == expected), "{expected} wasn't lowered");
    }
}
