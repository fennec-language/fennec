//! Lowering checked programs to the IR: the printed form is pinned for a few functions, and
//! everything lowered from every example has to pass the validator.

use std::fs;
use std::path::Path;

use fennec_check::Type;
use fennec_ir::facts::Facts;
use fennec_ir::{Block, Function, Local, LocalId, Operand, Program, Terminator, lower, validate};

fn lowered(source: &str) -> Result<Program, String> {
    let mut module = fennec_syntax::parse(source).map_err(|problem| problem.message)?;
    let checked = fennec_check::check_and_resolve(&mut module).map_err(|problems| format!("{problems:?}"))?;
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
fn something_generic_nothing_uses_isnt_lowered() {
    let source = "def first[T](items: list[T]) -> T:
    items[0]
";
    let program = lowered(source).unwrap();
    let why: Vec<String> =
        program.not_lowered.iter().map(|skipped| format!("{}: {}", skipped.function, skipped.why)).collect();
    assert_eq!(why, Vec::<String>::new());
}

#[test]
fn main_prints_through_its_capability() {
    let source = "def main(stdout: Stdout):
    name = \"fox\"
    stdout.print(f\"hello {name}\", name.upper())
";
    assert_eq!(
        printed(source, "main").unwrap(),
        "fn main(_0)
  local _0 stdout: Stdout
  local _1 name: str
  local _2: str
  local _3: str
  bb0:
    _1 = \"fox\"
    _2 = format \"hello \" {_1}
    _3 = builtin str.upper(_1)
    builtin Stdout.print(_0, _2, _3)
    return
"
    );
}

#[test]
fn lists_are_written_out_read_changed_and_looped_over() {
    let source = "def total(limit: int) -> int:
    mut items = [1, limit]
    items.append(3)
    items[0] += 1
    mut sum = 0
    for item in items:
        sum += item
    sum + items[1]
";
    assert_eq!(
        printed(source, "total").unwrap(),
        "fn total(_0) -> int
  local _0 limit: int
  local _1: list[int]
  local _2 items: list[int]
  local _3: int
  local _4: int
  local _5 sum: int
  local _6: list[int]
  local _7: int
  local _8: int
  local _9: int
  local _10: bool
  local _11 item: int
  local _12: int
  local _13: int
  bb0:
    _1 = [1, _0]
    _2 = _1
    builtin list.append(mut _2, 3)
    _3 = _2[0]
    _4 = _3 + 1
    _2[0] = _4
    _5 = 0
    _6 = _2
    _7 = builtin list.len(_6)
    _8 = 0
    _9 = _7
    jump bb1
  bb1:
    _10 = _8 < _9
    branch _10 bb2 bb4
  bb2:
    _11 = _6[_8]
    _5 = _5 + _11
    jump bb3
  bb3:
    _8 = _8 + 1
    jump bb1
  bb4:
    _12 = _2[1]
    _13 = _5 + _12
    return _13
"
    );
}

#[test]
fn a_mut_field_is_copied_in_and_back_out() {
    let source = "struct Bag:
    items: list[int]

    def add(mut self, item: int):
        self.items.append(item)

def grow(mut items: list[int]):
    items.append(1)

def fill(mut bag: Bag):
    grow(mut bag.items)
    bag.add(2)
";
    assert_eq!(
        printed(source, "Bag.add").unwrap(),
        "fn Bag.add(mut _0, _1)
  local _0 self: Bag
  local _1 item: int
  local _2: list[int]
  bb0:
    _2 = _0.items
    builtin list.append(mut _2, _1)
    _0.items = _2
    return
"
    );
    assert_eq!(
        printed(source, "fill").unwrap(),
        "fn fill(mut _0)
  local _0 bag: Bag
  local _1: list[int]
  bb0:
    _1 = _0.items
    call grow(_1)
    _0.items = _1
    call Bag.add(_0, 2)
    return
"
    );
}

#[test]
fn the_standard_librarys_fennec_functions_come_in_as_theyre_reached() {
    let source = "def starts(text: str) -> bool:
    text.startswith(\"a\")
";
    let program = lowered(source).unwrap();
    assert_eq!(program.external, Vec::<String>::new());
    assert_eq!(program.not_lowered, []);
    assert!(program.functions.iter().any(|function| function.name == "str.startswith"));
}

#[test]
fn in_and_not_in_ask_the_engines() {
    let source = "def has(items: list[int], text: str) -> bool:
    1 in items and \"a\" not in text
";
    let printed = printed(source, "has").unwrap();
    assert!(printed.contains("builtin list.contains(_0, 1)"), "{printed}");
    assert!(printed.contains("builtin str.contains(_1, \"a\")"), "{printed}");
}

#[test]
fn structs_are_built_read_and_changed() {
    let source = "struct Vec2:
    x: float
    y: float

struct Body:
    pos: Vec2
    mass: float

def moved(body: Body, by: float) -> Body:
    mut next = body
    next.pos.x = next.pos.x + by
    next.mass *= 2.0
    next
";
    assert_eq!(
        printed(source, "moved").unwrap(),
        "fn moved(_0, _1) -> Body
  local _0 body: Body
  local _1 by: float
  local _2 next: Body
  local _3: Vec2
  local _4: float
  local _5: float
  local _6: float
  local _7: float
  bb0:
    _2 = _0
    _3 = _2.pos
    _4 = _3.x
    _5 = _4 + _1
    _2.pos.x = _5
    _6 = _2.mass
    _7 = _6 * 2.0
    _2.mass = _7
    return _2
"
    );
}

#[test]
fn methods_take_their_object_first_and_arguments_land_in_their_fields() {
    let source = "struct Point:
    x: int
    y: int

    def origin() -> Point:
        Point(y=0, x=0)

    def sum(self) -> int:
        self.x + self.y

def total() -> int:
    Point.origin().sum()
";
    assert_eq!(
        printed(source, "Point.origin").unwrap(),
        "fn Point.origin() -> Point
  local _0: Point
  bb0:
    _0 = make Point(0, 0)
    return _0
"
    );
    assert_eq!(
        printed(source, "total").unwrap(),
        "fn total() -> int
  local _0: Point
  local _1: int
  bb0:
    _0 = call Point.origin()
    _1 = call Point.sum(_0)
    return _1
"
    );
}

#[test]
fn for_counts_in_a_local_of_its_own() {
    let source = "def total(n: int) -> int:
    mut sum = 0
    for i in range(n, 0, -1):
        if i == 3: continue
        sum += i
    sum
";
    assert_eq!(
        printed(source, "total").unwrap(),
        "fn total(_0) -> int
  local _0 n: int
  local _1 sum: int
  local _2: int
  local _3: int
  local _4: bool
  local _5 i: int
  local _6: bool
  bb0:
    _1 = 0
    _2 = _0
    _3 = 0
    jump bb1
  bb1:
    _4 = _2 > _3
    branch _4 bb2 bb4
  bb2:
    _5 = _2
    _6 = _5 == 3
    branch _6 bb6 bb7
  bb3:
    _2 = _2 - 1
    jump bb1
  bb4:
    return _1
  bb5:
    _1 = _1 + _5
    jump bb3
  bb6:
    jump bb3
  bb7:
    jump bb5
"
    );
}

const TOKENS: &str = "enum Tok:
    Int(value: int)
    Name(text: str)
    Eof

enum ExprKind:
    Int(value: int)
    Name(text: str)
    Negative(inner: int)

";

#[test]
fn a_leading_dot_case_takes_its_enum_from_the_typed_tree() {
    let source = format!(
        "{TOKENS}def literal(tok: Tok) -> ExprKind:
    match tok:
        case .Int(value):
            .Int(value)
        case .Name(text):
            .Name(text)
        case .Eof:
            ExprKind.Negative(0)
"
    );
    assert_eq!(
        printed(&source, "literal").unwrap(),
        "fn literal(_0) -> ExprKind
  local _0 tok: Tok
  local _1: bool
  local _2: int
  local _3 value: int
  local _4: ExprKind
  local _5: bool
  local _6: str
  local _7 text: str
  local _8: ExprKind
  local _9: bool
  local _10: ExprKind
  bb0:
    _1 = _0 is .Int
    branch _1 bb2 bb1
  bb1:
    _5 = _0 is .Name
    branch _5 bb4 bb3
  bb2:
    _2 = _0.Int.value
    _3 = _2
    _4 = make ExprKind.Int(_3)
    return _4
  bb3:
    _9 = _0 is .Eof
    branch _9 bb6 bb5
  bb4:
    _6 = _0.Name.text
    _7 = _6
    _8 = make ExprKind.Name(_7)
    return _8
  bb5:
    no match _0
  bb6:
    _10 = make ExprKind.Negative(0)
    return _10
"
    );
}

#[test]
fn alternatives_share_their_names_and_a_guard_falls_through() {
    let source = format!(
        "{TOKENS}def size(tok: Tok) -> int:
    match tok:
        case .Int(n) | .Name(n) if False:
            0
        case _:
            1
"
    )
    .replace("Name(text: str)\n    Eof", "Name(text: int)\n    Eof");
    assert_eq!(
        printed(&source, "size").unwrap(),
        "fn size(_0) -> int
  local _0 tok: Tok
  local _1: bool
  local _2: int
  local _3 n: int
  local _4: bool
  local _5: int
  bb0:
    _1 = _0 is .Int
    branch _1 bb4 bb3
  bb1:
    return 1
  bb2:
    branch false bb7 bb1
  bb3:
    _4 = _0 is .Name
    branch _4 bb6 bb5
  bb4:
    _2 = _0.Int.value
    _3 = _2
    jump bb2
  bb5:
    jump bb1
  bb6:
    _5 = _0.Name.text
    _3 = _5
    jump bb2
  bb7:
    return 0
"
    );
}

#[test]
fn a_match_can_be_a_value_and_values_can_be_tested() {
    let source = "def name(n: int) -> str:
    text = match n:
        case 0:
            \"none\"
        case _:
            \"some\"
    text + \"!\"
";
    let printed = printed(source, "name").unwrap();
    assert!(printed.contains("_2 = _0 == 0"), "{printed}");
    assert!(printed.contains("_1 = \"none\""), "{printed}");
    assert!(printed.contains("_1 = \"some\""), "{printed}");
}

#[test]
fn enum_methods_and_struct_patterns_lower() {
    let source = "struct Point:
    x: int
    y: int

enum Light:
    Red
    Green

    def next(self) -> Light:
        match self:
            case .Red:
                .Green
            case .Green:
                .Red

def x_of(point: Point) -> int:
    match point:
        case Point(x, _):
            x

def twice(light: Light) -> Light:
    light.next().next()
";
    let program = lowered(source).unwrap();
    assert_eq!(program.not_lowered, []);
    assert!(printed(source, "twice").unwrap().contains("_2 = call Light.next(_1)"));
    assert!(printed(source, "x_of").unwrap().contains("_1 = _0.x"));
}

const FILES: &str = "error Missing:
    path: str

error Denied:
    who: str

def load(path: str) -> str:
    if path == \"\":
        fail Missing(path)
    if path == \"root\":
        fail Denied(path)
    path

";

#[test]
fn fail_and_success_are_written_out() {
    let source = format!("{FILES}def main(stdout: Stdout):\n    stdout.print(1)\n");
    assert_eq!(
        printed(&source, "load").unwrap(),
        "fn load(_0) -> result of str
  local _0 path: str
  local _1: bool
  local _2: Missing
  local _3: result of str
  local _4: bool
  local _5: Denied
  local _6: result of str
  local _7: result of str
  bb0:
    _1 = _0 == \"\"
    branch _1 bb2 bb3
  bb1:
    _4 = _0 == \"root\"
    branch _4 bb5 bb6
  bb2:
    _2 = make Missing(_0)
    _3 = err _2
    return _3
  bb3:
    jump bb1
  bb4:
    _7 = ok _0
    return _7
  bb5:
    _5 = make Denied(_0)
    _6 = err _5
    return _6
  bb6:
    jump bb4
"
    );
}

#[test]
fn question_mark_leaves_with_the_error() {
    let source = format!("{FILES}def twice(path: str) -> str:\n    text = load(path)?\n    text + text\n");
    assert_eq!(
        printed(&source, "twice").unwrap(),
        "fn twice(_0) -> result of str
  local _0 path: str
  local _1: result of str
  local _2: bool
  local _3: Denied | Missing
  local _4: result of str
  local _5: str
  local _6 text: str
  local _7: str
  local _8: result of str
  bb0:
    _1 = call load(_0)
    _2 = _1 is err
    branch _2 bb1 bb2
  bb1:
    _3 = _1 as err
    _4 = err _3
    return _4
  bb2:
    _5 = _1 as ok
    _6 = _5
    _7 = _6 + _6
    _8 = ok _7
    return _8
"
    );
}

#[test]
fn matching_a_result_sees_each_error_and_looks_through_ok() {
    let source = format!(
        "{FILES}def show(path: str) -> str:
    match load(path):
        case Missing(name):
            name
        case Err(_):
            \"denied\"
        case text:
            text
"
    );
    let printed = printed(&source, "show").unwrap();
    for line in ["is Missing", "as error", ".path", "as err", "as ok"] {
        assert!(printed.contains(line), "no {line} in {printed}");
    }
}

#[test]
fn a_name_known_not_to_be_none_is_unwrapped_where_its_read() {
    let source = "def next(count: int?) -> int:
    if count is None:
        return 0
    count + 1
";
    assert_eq!(
        printed(source, "next").unwrap(),
        "fn next(_0) -> int
  local _0 count: int?
  local _1: bool
  local _2: int
  local _3: int
  bb0:
    _1 = _0 is none
    branch _1 bb2 bb3
  bb1:
    _2 = _0 as some
    _3 = _2 + 1
    return _3
  bb2:
    return 0
  bb3:
    jump bb1
"
    );
}

#[test]
fn values_are_wrapped_going_where_an_optional_goes_and_else_runs_only_when_needed() {
    let source = "struct Slot:
    held: int?

def fill(mut slot: Slot, value: int?) -> int:
    slot.held = 1
    slot.held = None
    mut kept: int? = 2
    kept = None
    value else 3
";
    let printed = printed(source, "fill").unwrap();
    for line in ["= some 1", "_0.held = none", "= some 2", "_3 = none", "is none", "as some"] {
        assert!(printed.contains(line), "no {line} in {printed}");
    }
}

#[test]
fn matching_an_optional_tests_for_none_first() {
    let source = "def name(code: int?) -> str:
    match code:
        case None:
            \"none\"
        case 1 | 2:
            \"low\"
        case n:
            \"other\"
";
    assert_eq!(lowered(source).unwrap().not_lowered, []);
}

/// A one-block function with the given locals, ending as given.
fn function(locals: Vec<Type>, end: Terminator) -> Function {
    Function {
        name: "f".to_owned(),
        params: 0,
        mut_params: Vec::new(),
        returns: Type::Int,
        locals: locals.into_iter().map(|ty| Local { name: None, ty }).collect(),
        blocks: vec![Block { insts: Vec::new(), end }],
        counts: Vec::new(),
    }
}

#[test]
fn the_validator_finds_malformed_ir() {
    let problems = |function: Function| {
        validate(&Program { functions: vec![function], ..Program::default() }).unwrap_err()
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
fn a_comprehension_is_a_loop_that_fills_a_collection() {
    let source = "def evens(xs: list[int]) -> list[int]:\n    [x * 2 for x in xs if x > 0]\n";
    assert_eq!(
        printed(source, "evens").unwrap(),
        "fn evens(_0) -> list[int]
  local _0 xs: list[int]
  local _1: list[int]
  local _2: list[int]
  local _3: int
  local _4: int
  local _5: int
  local _6: bool
  local _7 x: int
  local _8: bool
  local _9: int
  bb0:
    _1 = []
    _2 = _0
    _3 = builtin list.len(_2)
    _4 = 0
    _5 = _3
    jump bb1
  bb1:
    _6 = _4 < _5
    branch _6 bb2 bb4
  bb2:
    _7 = _2[_4]
    _8 = _7 > 0
    branch _8 bb5 bb3
  bb3:
    _4 = _4 + 1
    jump bb1
  bb4:
    return _1
  bb5:
    _9 = _7 * 2
    builtin list.append(mut _1, _9)
    jump bb3
"
    );
}

#[test]
fn dict_and_set_comprehensions() {
    let source = "def f(xs: list[int]) -> dict[int, str]:\n    {x: str(x) for x in {x for x in xs}}\n";
    let text = printed(source, "f").unwrap();
    for expected in ["= dict {}", "= set {}", "builtin set.add(mut", "builtin list(", "] = "] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn tuples_unpack_in_assignments_and_loops() {
    let source = "def f(pairs: dict[str, int]) -> int:
    mut total = 0
    for key, value in pairs.items():
        total += value + key.len()
    mut a = total
    mut b = 1
    a, b = (b, a)
    a + (a, b)[1]
";
    let text = printed(source, "f").unwrap();
    for expected in ["builtin dict.items(", "key: str", "= tuple (", ".0", ".1"] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn dicts_and_slices_read_and_change_items() {
    let source = "def f(xs: list[int], d: dict[str, int?]) -> list[int]:
    mut seen = d
    seen[\"a\"] = 1
    if \"b\" in seen:
        return xs[1:]
    xs[:2]
";
    let text = printed(source, "f").unwrap();
    for expected in ["= some 1", "[\"a\"] = ", "builtin dict.contains(", "= _0[1:]", "= _0[:2]"] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn a_types_own_operators_are_calls() {
    let source = "struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        Money(self.cents + other.cents)

    def <(self, other: Money) -> bool:
        self.cents < other.cents

def f(mut a: Money, b: Money) -> bool:
    a += b
    a >= b
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(mut _0, _1) -> bool
  local _0 a: Money
  local _1 b: Money
  local _2: Money
  local _3: bool
  local _4: bool
  bb0:
    _2 = call Money.+(_0, _1)
    _0 = _2
    _3 = call Money.<(_0, _1)
    _4 = not _3
    return _4
"
    );
}

#[test]
fn equality_works_on_every_value() {
    let source = "struct P:
    x: int

def f(a: P, b: P, c: list[int], d: int?) -> bool:
    a == b and c != [1] and d == 3 and c < c
";
    let text = printed(source, "f").unwrap();
    for expected in ["= _0 == _1", "!= ", "= some 3", " < _2"] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn named_arguments_land_in_their_parameters_and_run_in_order() {
    let source = "def g(a: int, b: str) -> int:
    a + b.len()

def h() -> int:
    1

def f() -> int:
    g(b=str(h()), a=h())
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("_2 = call h()"), "{text}");
    assert!(text.contains("call g(_2, _1)"), "{text}");
}

#[test]
fn fields_left_out_get_their_defaults() {
    let source = "struct P:
    x: int
    y = 2.5
    z: int? = 3

def f() -> P:
    P(1)
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("= some 3"), "{text}");
    assert!(text.contains("= make P(1, 2.5, _0)"), "{text}");
}

#[test]
fn literal_constants_are_written_out_and_others_are_functions() {
    let source = "LIMIT = -2
WORDS = [\"a\", \"b\"]
DOUBLE = LIMIT * 2

def f() -> int:
    WORDS.len() + LIMIT + DOUBLE
";
    let program = lowered(source).unwrap();
    let names: Vec<&str> = program.constants.iter().map(|constant| constant.name.as_str()).collect();
    assert_eq!(names, ["DOUBLE", "WORDS"]);
    let text = printed(source, "f").unwrap();
    assert!(text.contains("= constant WORDS"), "{text}");
    assert!(text.contains("+ -2"), "{text}");
    assert!(text.contains("= constant DOUBLE"), "{text}");
}

#[test]
fn what_std_provides_goes_to_the_engines() {
    let source = "from std.math import PI, sqrt

def f(x: float) -> float:
    sqrt(x) * PI
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("builtin math.sqrt(_0)"), "{text}");
    assert!(text.contains("builtin PI()"), "{text}");
}

#[test]
fn a_range_with_a_step_goes_through_a_list() {
    let source = "def f(n: int) -> int:
    mut total = 0
    for i in range(0, n, 2):
        total += i
    total
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("builtin range(0, _0, 2)"), "{text}");
}

#[test]
fn for_mut_writes_each_item_back_however_the_loop_is_left() {
    let source = "def f(mut xs: list[int]) -> int:
    for mut x in xs:
        x += 1
        if x > 3:
            break
        if x < 0:
            return x
    0
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(mut _0) -> int
  local _0 xs: list[int]
  local _1: list[int]
  local _2: int
  local _3: int
  local _4: int
  local _5: bool
  local _6 x: int
  local _7: bool
  local _8: bool
  local _9: int
  bb0:
    _1 = _0
    _2 = builtin list.len(_1)
    _3 = 0
    _4 = _2
    jump bb1
  bb1:
    _5 = _3 < _4
    branch _5 bb2 bb4
  bb2:
    _6 = _1[_3]
    _6 = _6 + 1
    _7 = _6 > 3
    branch _7 bb7 bb8
  bb3:
    _0[_3] = _6
    _3 = _3 + 1
    jump bb1
  bb4:
    return 0
  bb5:
    _0[_3] = _6
    jump bb4
  bb6:
    _8 = _6 < 0
    branch _8 bb10 bb11
  bb7:
    jump bb5
  bb8:
    jump bb6
  bb9:
    jump bb3
  bb10:
    _9 = _6
    _0[_3] = _6
    return _9
  bb11:
    jump bb9
"
    );
}

#[test]
fn changing_an_items_insides_reads_it_changes_it_and_writes_it_back() {
    let source = "struct P:
    x: int

def f(mut ps: list[P], mut grid: list[list[int]]):
    ps[0].x += 1
    grid[1][2] = 3
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(mut _0, mut _1)
  local _0 ps: list[P]
  local _1 grid: list[list[int]]
  local _2: P
  local _3: int
  local _4: int
  local _5: P
  local _6: list[int]
  bb0:
    _2 = _0[0]
    _3 = _2.x
    _4 = _3 + 1
    _5 = _0[0]
    _5.x = _4
    _0[0] = _5
    _6 = _1[1]
    _6[2] = 3
    _1[1] = _6
    return
"
    );
}

#[test]
fn a_types_own_indexing_is_calls() {
    let source = "struct Grid:
    cells: list[int]

    def [](self, at: int) -> int:
        self.cells[at]

    def []=(mut self, at: int, value: int):
        self.cells[at] = value

struct Holder:
    grid: Grid

def f(mut holder: Holder) -> int:
    holder.grid[1] = 2
    holder.grid[0]
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(mut _0) -> int
  local _0 holder: Holder
  local _1: Grid
  local _2: Grid
  local _3: int
  bb0:
    _1 = _0.grid
    call Grid.[]=(_1, 1, 2)
    _0.grid = _1
    _2 = _0.grid
    _3 = call Grid.[](_2, 0)
    return _3
"
    );
}

#[test]
fn mut_arguments_can_be_items() {
    let source = "def bump(mut n: int):
    n += 1

def f(mut xs: list[int], mut lists: list[list[int]]):
    bump(mut xs[0])
    lists[1].append(2)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(mut _0, mut _1)
  local _0 xs: list[int]
  local _1 lists: list[list[int]]
  local _2: int
  local _3: list[int]
  bb0:
    _2 = _0[0]
    call bump(_2)
    _0[0] = _2
    _3 = _1[1]
    builtin list.append(mut _3, 2)
    _1[1] = _3
    return
"
    );
}

#[test]
fn falling_off_the_end_of_a_function_that_can_fail_succeeds() {
    let source = "error Bad:
    code: int

def f(x: int):
    if x < 0:
        fail Bad(x)
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("= ok\n    return _"), "{text}");
    assert!(!text.contains("unreachable"), "{text}");
}

#[test]
fn a_types_own_index_can_give_a_str() {
    let source = "struct Names:
    all: list[str]

    def [](self, at: int) -> str:
        self.all[at]

def f(names: Names) -> str:
    names[0]
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("call Names.[](_0, 0)"), "{text}");
}

#[test]
fn an_arrow_function_is_lifted_with_what_it_captures() {
    let source = "def apply(value: int, change: (int) -> int) -> int:
    change(value)

def f(x: int) -> int:
    offset = x + 1
    apply(x, (n) => n + offset)
";
    let program = lowered(source).unwrap();
    let shown: Vec<String> = program.functions.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown.join(""),
        "fn apply(_0, _1) -> int
  local _0 value: int
  local _1 change: (int) -> int
  local _2: int
  bb0:
    _2 = call closure _1(_0)
    return _2
fn f(_0) -> int
  local _0 x: int
  local _1: int
  local _2 offset: int
  local _3: (int) -> int
  local _4: int
  bb0:
    _1 = _0 + 1
    _2 = _1
    _3 = closure f.arrow@125[_2]
    _4 = call apply(_0, _3)
    return _4
fn f.arrow@125(_0, _1) -> int
  local _0 offset: int
  local _1 n: int
  local _2: int
  bb0:
    _2 = _1 + _0
    return _2
"
    );
}

#[test]
fn a_nested_function_captures_and_can_call_itself() {
    let source = "def f(start: int) -> int:
    def down(n: int) -> int:
        if n <= start:
            return n
        down(n - 1)
    down(10)
";
    let program = lowered(source).unwrap();
    let shown: Vec<String> = program.functions.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown.join(""),
        "fn f(_0) -> int
  local _0 start: int
  local _1: (int) -> int
  local _2 down: (int) -> int
  local _3: int
  bb0:
    _1 = closure f.down[_0]
    _2 = _1
    _3 = call closure _2(10)
    return _3
fn f.down(_0, _1) -> int
  local _0 start: int
  local _1 n: int
  local _2 down: (int) -> int
  local _3: bool
  local _4: int
  local _5: int
  bb0:
    _2 = closure f.down[_0]
    _3 = _1 <= _0
    branch _3 bb2 bb3
  bb1:
    _4 = _1 - 1
    _5 = call closure _2(_4)
    return _5
  bb2:
    return _1
  bb3:
    jump bb1
"
    );
}

#[test]
fn a_function_by_name_is_a_closure_of_nothing() {
    let source = "def double(n: int) -> int:
    n * 2

def f() -> int:
    g = double
    g(2)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f() -> int
  local _0: (int) -> int
  local _1 g: (int) -> int
  local _2: int
  bb0:
    _0 = closure double[]
    _1 = _0
    _2 = call closure _1(2)
    return _2
"
    );
}

#[test]
fn a_key_goes_last_and_names_the_call() {
    let source = "def f(xs: list[int]) -> list[int]:
    sorted(xs, key=(n) => -n)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> list[int]
  local _0 xs: list[int]
  local _1: (int) -> int
  local _2: list[int]
  bb0:
    _1 = closure f.arrow@54[]
    _2 = builtin sorted.key(_0, _1)
    return _2
"
    );
}

#[test]
fn a_tuple_of_one_type_and_the_arguments_index_by_a_value() {
    let source = "def f(pair: (int, int), at: int) -> int:
    pair[at]

def g(args: Args, at: int) -> str:
    args[at]

def h(data: bytes, at: int) -> int:
    data[at]
";
    assert!(printed(source, "f").unwrap().contains("= _0[_1]"));
    assert!(printed(source, "g").unwrap().contains("= _0[_1]"));
    assert!(printed(source, "h").unwrap().contains("= _0[_1]"));
}

#[test]
fn a_loop_over_a_generator_with_a_cleanup_or_match_still_lowers() {
    let source = "struct Door:
    open: bool

    def del(self):
        pass

def held() -> Iterator[int]:
    with Door(open=True) as door:
        yield 1
    match 2:
        case 2:
            yield 2
        case _:
            return

def f() -> int:
    sum(n for n in held())
";
    assert!(printed(source, "held.resume").is_ok());
    let closing = printed(source, "held.abandon").unwrap();
    assert!(closing.contains("call Door.del"), "{closing}");
    let module = fennec_syntax::parse(source).unwrap();
    let checked = fennec_check::check(&module).unwrap();
    let program = lower(&module, &checked);
    let generators: Vec<_> =
        program.generators.iter().map(|def| (&*def.name, def.abandon.as_deref())).collect();
    assert_eq!(generators, [("held", Some("held.abandon"))]);
    assert!(!printed(source, "f").unwrap().contains("list.append"));
}

#[test]
fn a_loop_over_a_type_goes_through_what_its_iter_gives() {
    let source = "struct Bag:
    all: list[int]

    def iter(self) -> Iterator[int]:
        for one in self.all:
            yield one

def f(bag: Bag) -> list[int]:
    [one for one in bag]
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("= call Bag.iter(_0)"), "{text}");
}

#[test]
fn a_tuple_pattern_tests_each_part() {
    let source = "def f(pair: (int, str)) -> int:
    match pair:
        case (0, _):
            1
        case (x, _):
            x
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> int
  local _0 pair: (int, str)
  local _1: int
  local _2: bool
  local _3: int
  local _4 x: int
  bb0:
    _1 = _0.0
    _2 = _1 == 0
    branch _2 bb2 bb1
  bb1:
    _3 = _0.0
    _4 = _3
    return _4
  bb2:
    return 1
"
    );
}

#[test]
fn an_else_chain_gives_the_default_for_none() {
    let source = "struct P:
    level: int

def f(p: P?) -> int:
    p.level else 0
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> int
  local _0 p: P?
  local _1: int?
  local _2: bool
  local _3: P
  local _4: int
  local _5: int?
  local _6: bool
  local _7: int
  local _8: int
  bb0:
    _2 = _0 is none
    branch _2 bb1 bb2
  bb1:
    _1 = none
    jump bb3
  bb2:
    _3 = _0 as some
    _4 = _3.level
    _5 = some _4
    _1 = _5
    jump bb3
  bb3:
    _6 = _1 is none
    branch _6 bb4 bb5
  bb4:
    _7 = 0
    jump bb6
  bb5:
    _8 = _1 as some
    _7 = _8
    jump bb6
  bb6:
    return _7
"
    );
}

#[test]
fn a_where_rule_gives_an_error_unless_the_values_are_written_out() {
    let source = "struct Score:
    value: int where value >= 0

def f(n: int) -> Score:
    Score(n)?

def g() -> Score:
    Score(1)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> result of Score
  local _0 n: int
  local _1: result of Score
  local _2 value: int
  local _3: bool
  local _4: InvalidValue
  local _5: Score
  local _6: bool
  local _7: InvalidValue
  local _8: result of Score
  local _9: Score
  local _10: result of Score
  bb0:
    _2 = _0
    _3 = _2 >= 0
    branch _3 bb2 bb3
  bb1:
    _6 = _1 is err
    branch _6 bb4 bb5
  bb2:
    _5 = make Score(_0)
    _1 = ok _5
    jump bb1
  bb3:
    _4 = make InvalidValue(\"Score.value breaks its `where` rule\")
    _1 = err _4
    jump bb1
  bb4:
    _7 = _1 as err
    _8 = err _7
    return _8
  bb5:
    _9 = _1 as ok
    _10 = ok _9
    return _10
"
    );
    assert_eq!(
        printed(source, "g").unwrap(),
        "fn g() -> Score
  local _0 value: int
  local _1: bool
  local _2: Score
  bb0:
    _0 = 1
    _1 = _0 >= 0
    branch _1 bb2 bb3
  bb1:
    return _2
  bb2:
    _2 = make Score(1)
    jump bb1
  bb3:
    stop \"this breaks the `where` rule on `Score.value`\"
"
    );
}

#[test]
fn std_module_calls_go_to_the_engines_by_their_full_name() {
    let source = "from std import json as data
import std.math as geometry
from std.toml import loads as toml_loads

def f(text: str) -> list[int]:
    a = geometry.sqrt(16.0)
    b = data.dumps(a)
    t = toml_loads[dict[str, int]](text)?
    data.loads[list[int]](b)? + t.values()
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> result of list[int]
  local _0 text: str
  local _1: float
  local _2 a: float
  local _3: str
  local _4 b: str
  local _5: result of dict[str, int]
  local _6: bool
  local _7: ParseError
  local _8: result of list[int]
  local _9: dict[str, int]
  local _10 t: dict[str, int]
  local _11: result of list[int]
  local _12: bool
  local _13: ParseError
  local _14: result of list[int]
  local _15: list[int]
  local _16: list[int]
  local _17: list[int]
  local _18: result of list[int]
  bb0:
    _1 = builtin math.sqrt(16.0)
    _2 = _1
    _3 = builtin json.dumps(_2)
    _4 = _3
    _5 = builtin toml.loads(_0)
    _6 = _5 is err
    branch _6 bb1 bb2
  bb1:
    _7 = _5 as err
    _8 = err _7
    return _8
  bb2:
    _9 = _5 as ok
    _10 = _9
    _11 = builtin json.loads(_4)
    _12 = _11 is err
    branch _12 bb3 bb4
  bb3:
    _13 = _11 as err
    _14 = err _13
    return _14
  bb4:
    _15 = _11 as ok
    _16 = builtin dict.values(_10)
    _17 = _15 + _16
    _18 = ok _17
    return _18
"
    );
}

#[test]
fn an_id_map_is_read_and_changed_by_id() {
    let source = "from std.game import IdMap

struct Enemy:
    health: int

    def hit(mut self):
        self.health -= 1

def f() -> int:
    mut map = IdMap[Enemy]()
    one = map.insert(Enemy(2))
    map[one] = Enemy(3)
    map.modify(one, e => e.hit())
    for mut enemy in map:
        enemy.hit()
    map[one].health
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f() -> int
  local _0: IdMap[Enemy]
  local _1 map: IdMap[Enemy]
  local _2: Enemy
  local _3: Id[Enemy]
  local _4 one: Id[Enemy]
  local _5: Enemy
  local _6: (Enemy) -> nothing
  local _7: list[(Id[Enemy], Enemy)]
  local _8: int
  local _9: int
  local _10: int
  local _11: bool
  local _12: (Id[Enemy], Enemy)
  local _13: Id[Enemy]
  local _14 enemy: Enemy
  local _15: Enemy
  local _16: int
  bb0:
    _0 = builtin IdMap()
    _1 = _0
    _2 = make Enemy(2)
    _3 = builtin IdMap.insert(mut _1, _2)
    _4 = _3
    _5 = make Enemy(3)
    _1[_4] = _5
    _6 = closure f.arrow@228[]
    builtin IdMap.modify(mut _1, _4, _6)
    _7 = builtin IdMap.items(_1)
    _8 = builtin list.len(_7)
    _9 = 0
    _10 = _8
    jump bb1
  bb1:
    _11 = _9 < _10
    branch _11 bb2 bb4
  bb2:
    _12 = _7[_9]
    _13 = _12.0
    _14 = _12.1
    call Enemy.hit(_14)
    jump bb3
  bb3:
    _1[_13] = _14
    _9 = _9 + 1
    jump bb1
  bb4:
    _15 = _1[_4]
    _16 = _15.health
    return _16
"
    );
}

#[test]
fn with_cleans_up_however_its_block_is_left() {
    let source = "struct File:
    name: str

    def del(self):
        pass

def f(n: int) -> int:
    while n > 0:
        with File(\"a\") as file:
            if n == 1:
                break
            if n == 2:
                return file.name.len()
    with File(\"b\"):
        pass
    0
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> int
  local _0 n: int
  local _1: bool
  local _2: File
  local _3 file: File
  local _4: bool
  local _5: bool
  local _6: str
  local _7: int
  local _8: int
  local _9: File
  local _10: File
  bb0:
    jump bb1
  bb1:
    _1 = _0 > 0
    branch _1 bb2 bb3
  bb2:
    _2 = make File(\"a\")
    _3 = _2
    _4 = _0 == 1
    branch _4 bb5 bb6
  bb3:
    _9 = make File(\"b\")
    _10 = _9
    call File.del(_10)
    return 0
  bb4:
    _5 = _0 == 2
    branch _5 bb8 bb9
  bb5:
    call File.del(_3)
    jump bb3
  bb6:
    jump bb4
  bb7:
    call File.del(_3)
    jump bb1
  bb8:
    _6 = _3.name
    _7 = builtin str.len(_6)
    _8 = _7
    call File.del(_3)
    return _8
  bb9:
    jump bb7
"
    );
}

#[test]
fn an_assert_stops_the_program() {
    let source = "def f(n: int):\n    assert n > 0\n";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0)
  local _0 n: int
  local _1: bool
  bb0:
    _1 = _0 > 0
    branch _1 bb1 bb2
  bb1:
    return
  bb2:
    stop \"assertion failed\"
"
    );
}

#[test]
fn an_else_chain_call_runs_only_on_a_value() {
    let source = "struct P:
    level: int

    def up(self) -> int:
        self.level + 1

def f(p: P?) -> int:
    p.up() else 0
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> int
  local _0 p: P?
  local _1: int?
  local _2: bool
  local _3 ?.: P
  local _4: int
  local _5: int?
  local _6: bool
  local _7: int
  local _8: int
  bb0:
    _2 = _0 is none
    branch _2 bb1 bb2
  bb1:
    _1 = none
    jump bb3
  bb2:
    _3 = _0 as some
    _4 = call P.up(_3)
    _5 = some _4
    _1 = _5
    jump bb3
  bb3:
    _6 = _1 is none
    branch _6 bb4 bb5
  bb4:
    _7 = 0
    jump bb6
  bb5:
    _8 = _1 as some
    _7 = _8
    jump bb6
  bb6:
    return _7
"
    );
}

#[test]
fn replace_copies_and_changes_fields() {
    let source = "struct P:
    x: int
    y: int? = None

def f(p: P) -> P:
    replace(p, y=1)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> P
  local _0 p: P
  local _1: P
  local _2: int?
  bb0:
    _1 = _0
    _2 = some 1
    _1.y = _2
    return _1
"
    );
}

#[test]
fn a_method_changing_a_name_known_not_to_be_none_writes_back_an_optional() {
    let source = "struct P:
    x: int

    def up(mut self):
        self.x += 1

def f(p: P?) -> P?:
    mut q = p
    if q is None:
        return None
    q.up()
    q
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> P?
  local _0 p: P?
  local _1 q: P?
  local _2: bool
  local _3: P
  local _4: P?
  local _5: P
  local _6: P?
  bb0:
    _1 = _0
    _2 = _1 is none
    branch _2 bb2 bb3
  bb1:
    _3 = _1 as some
    call P.up(_3)
    _4 = some _3
    _1 = _4
    _5 = _1 as some
    _6 = some _5
    return _6
  bb2:
    return none
  bb3:
    jump bb1
"
    );
}

#[test]
fn named_arguments_to_builtins_name_the_call() {
    let source = "from std import json

def f(xs: list[int]) -> str:
    json.dumps(xs, indent=2)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> str
  local _0 xs: list[int]
  local _1: str
  bb0:
    _1 = builtin json.dumps.indent(_0, 2)
    return _1
"
    );
}

#[test]
fn what_std_writes_in_fennec_is_lowered_with_its_types() {
    let source = "from std.math import Vec3

struct Body:
    position = Vec3.zero

def f() -> float:
    (Body().position + Vec3(x=3.0)).length()
";
    let program = lowered(source).unwrap();
    assert_eq!(program.external, Vec::<String>::new());
    for needed in ["Vec3.zero", "Vec3.+", "Vec3.length", "Vec3.dot"] {
        assert!(program.functions.iter().any(|function| function.name == needed), "{needed}");
    }
}

#[test]
fn a_generic_function_is_copied_for_each_type_its_used_with() {
    let source = "def first[T](items: list[T]) -> T:
    items[0]

def f() -> str:
    a = first([1])
    b = first([\"x\"])
    c = first([2])
    f\"{a} {b} {c}\"
";
    let program = lowered(source).unwrap();
    let names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    assert_eq!(names, ["f", "first[str]", "first[int]"]);
    let text = printed(source, "first[str]").unwrap();
    assert_eq!(
        text,
        "fn first[str](_0) -> str
  local _0 items: list[str]
  local _1: str
  bb0:
    _1 = _0[0]
    return _1
"
    );
}

#[test]
fn an_interface_calls_the_method_of_the_type_it_stands_for() {
    let source = "interface Shape:
    def area(self) -> float

struct Square:
    side: float

    def area(self) -> float:
        self.side * self.side

def twice[T: Shape](shape: T) -> float:
    shape.area() * 2.0

def f() -> float:
    twice(Square(1.0))
";
    assert_eq!(
        printed(source, "twice[Square]").unwrap(),
        "fn twice[Square](_0) -> float
  local _0 shape: Square
  local _1: float
  local _2: float
  bb0:
    _1 = call Square.area(_0)
    _2 = _1 * 2.0
    return _2
"
    );
}

#[test]
fn a_generic_struct_is_a_struct_for_each_type_it_holds() {
    let source = "struct Box[T]:
    held: T
    count = 0

    def get(self) -> T:
        self.held

def f() -> int:
    a = Box(1)
    b = Box(\"x\")
    a.get() + b.get().len()
";
    let program = lowered(source).unwrap();
    let shown: Vec<String> = program
        .structs
        .iter()
        .filter(|def| def.name.starts_with("Box"))
        .map(|def| format!("{def:?}"))
        .collect();
    assert_eq!(
        shown.join(", "),
        "StructDef { name: \"Box[int]\", fields: [(\"held\", Int), (\"count\", Int)] }, \
         StructDef { name: \"Box[str]\", fields: [(\"held\", Str), (\"count\", Int)] }"
    );
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f() -> int
  local _0: Box[int]
  local _1 a: Box[int]
  local _2: Box[str]
  local _3 b: Box[str]
  local _4: int
  local _5: str
  local _6: int
  local _7: int
  bb0:
    _0 = make Box[int](1, 0)
    _1 = _0
    _2 = make Box[str](\"x\", 0)
    _3 = _2
    _4 = call Box.get[int](_1)
    _5 = call Box.get[str](_3)
    _6 = builtin str.len(_5)
    _7 = _4 + _6
    return _7
"
    );
}

#[test]
fn a_method_of_a_list_is_copied_for_what_it_holds() {
    let source = "def f(mut xs: list[int]) -> int:
    xs.reverse()
    xs.count(1)
";
    let program = lowered(source).unwrap();
    let names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    assert_eq!(names, ["f", "list.count[int]", "list.reverse[int]"]);
}

#[test]
fn a_nested_function_in_a_generic_uses_what_its_types_stand_for() {
    let source = "def f[T](x: T) -> list[T]:
    def again() -> list[T]:
        [x, x]
    again()

def h() -> list[int]:
    f(1)
";
    let program = lowered(source).unwrap();
    let names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    assert!(names.contains(&"f[int].again"), "{names:?}");
}

#[test]
fn a_generic_that_calls_itself_with_a_bigger_type_stops() {
    let source = "def grow[T](x: T, n: int) -> int:
    if n == 0:
        return 0
    grow([x], n - 1)

def f() -> int:
    grow(1, 3)
";
    let program = lowered(source).unwrap();
    assert_ne!(program.not_lowered, []);
    assert!(program.functions.len() <= 2001, "{}", program.functions.len());
}

#[test]
fn lowering_some_starts_only_from_what_it_picks() {
    let source = "LIST = [1]

def a() -> int:
    b()

def b() -> int:
    LIST[0]

def c() -> int:
    1
";
    let module = fennec_syntax::parse(source).unwrap();
    let checked = fennec_check::check(&module).unwrap();
    let program = fennec_ir::lower_some(&module, &checked, &|key| key == "a");
    let names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    assert_eq!(names, ["a"]);
    assert_eq!(program.external, ["b"]);
    assert_eq!(program.constants, []);
}

#[test]
fn facts_narrow_a_local_holding_one_value_along_branches() {
    let source = "def f(n: int) -> int:
    if n < 2:
        return n
    n - 1
";
    let program = lowered(source).unwrap();
    let function = program.functions.first().unwrap();
    let facts = Facts::of(function, &|_| Vec::new());
    let n = LocalId(0);
    assert_eq!(facts.range_in(n, 0), Some((i64::MIN, i64::MAX)));
    let known: Vec<_> = (0..function.blocks.len()).filter_map(|at| facts.range_in(n, at)).collect();
    assert!(known.contains(&(i64::MIN, 1)), "{known:?}");
    assert!(known.contains(&(2, i64::MAX)), "{known:?}");
}

#[test]
fn facts_keep_a_count_inside_its_loop_and_say_nothing_of_a_changed_local() {
    let source = "def bump(mut x: int):
    x += 1

def f(n: int) -> int:
    mut total = 0
    bump(mut total)
    once = 5
    for i in range(3, 9):
        total += i + once
    total
";
    let program = lowered(source).unwrap();
    let function = program.functions.iter().find(|function| function.name == "f").unwrap();
    let mut facts = Facts::of(function, &|callee| if callee == "bump" { vec![0] } else { Vec::new() });
    let count = function.counts.first().unwrap();
    facts.note_write(count.stop, (9, 9));
    facts.note_write(count.count, (3, 3));
    facts.note_write(count.count, (4, 4));
    assert_eq!(facts.range_in(count.count, 0), Some((3, 8)));
    let total = function.locals.iter().position(|local| local.name.as_deref() == Some("total")).unwrap();
    facts.note_write(LocalId(total), (0, 0));
    assert_eq!(facts.range_in(LocalId(total), 0), None);
    let once = function.locals.iter().position(|local| local.name.as_deref() == Some("once")).unwrap();
    facts.note_write(LocalId(once), (5, 5));
    assert_eq!(facts.range_in(LocalId(once), 0), Some((5, 5)));
}

#[test]
fn a_write_runs_only_the_rule_of_the_field_it_writes() {
    let source = "struct Pair:
    low: int where low >= 0
    high: int where high >= 9

def f(mut pair: Pair):
    pair.low = 1
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("`Pair.low`"), "{text}");
    assert!(!text.contains("`Pair.high`"), "{text}");
}

#[test]
fn every_example_kit_lowers_to_valid_ir() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/projects");
    let mut kits = 0;
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.join("kit.toml").exists() {
            continue;
        }
        kits += 1;
        let shown = path.display().to_string();
        let mut kit = fennec_kit::load(&path, &shown, fennec_kit::Entry::Main).unwrap();
        let checked = fennec_check::check_and_resolve(&mut kit.module).unwrap();
        let program = lower(&kit.module, &checked);
        validate(&program).unwrap_or_else(|problems| panic!("{shown}: {problems:?}\n{program}"));
        assert_eq!(program.not_lowered, [], "{shown}");
        assert_eq!(program.external, Vec::<String>::new(), "{shown}");
    }
    assert!(kits >= 6, "only {kits} kits");
}

#[test]
fn every_example_lowers_to_valid_ir() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut lowered_functions = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "fen") {
            continue;
        }
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = fs::read_to_string(&path).unwrap();
        let mut module = fennec_syntax::parse(&source).unwrap();
        let checked = fennec_check::check_and_resolve(&mut module)
            .unwrap_or_else(|problems| panic!("{file}: {problems:?}"));
        let program = lower(&module, &checked);
        validate(&program).unwrap_or_else(|problems| panic!("{file}: {problems:?}\n{program}"));
        assert_eq!(program.not_lowered, [], "{file}");
        assert_eq!(program.external, Vec::<String>::new(), "{file}");
        lowered_functions
            .extend(program.functions.iter().map(|function| format!("{file}: {}", function.name)));
    }
    for expected in
        ["numbers.fen: fib", "numbers.fen: collatz_steps", "numbers.fen: is_prime", "numbers.fen: lerp"]
    {
        assert!(lowered_functions.iter().any(|found| found == expected), "{expected} wasn't lowered");
    }
}

#[test]
fn a_loop_over_a_stage_is_a_plain_loop() {
    let source = "def upto(limit: int) -> Iterator[int]:
    mut n = 0
    while n < limit:
        yield n
        n += 1

def odd(numbers: Iterator[int]) -> Iterator[int]:
    for n in numbers:
        if n % 2 == 1:
            yield n

def total(limit: int) -> int:
    mut sum_so_far = 0
    for n in odd(upto(limit)):
        sum_so_far += n * 2
    sum_so_far

def doubled(limit: int) -> list[int]:
    [n for n in (x * 2 for x in upto(limit)) if n > 2]
";
    let total = printed(source, "total").unwrap();
    assert!(!total.contains("call "), "{total}");
    assert!(!total.contains("list.append"), "{total}");
    let doubled = printed(source, "doubled").unwrap();
    assert!(!doubled.contains("call "), "{doubled}");
    assert_eq!(doubled.matches("list.append").count(), 1, "{doubled}");
}

#[test]
fn what_uses_a_stage_in_place_collects_nothing() {
    let source = "def upto(limit: int) -> Iterator[int]:
    mut n = 0
    while n < limit:
        yield n
        n += 1

def total(limit: int) -> int:
    sum(n * 2 for n in upto(limit) if n > 1)

def scaled(xs: list[float]) -> float:
    sum(x * 2.0 for x in xs)

def has_big(limit: int) -> bool:
    any(n > 2 for n in upto(limit))

def all_small(xs: list[int]) -> bool:
    all(x < 2 for x in xs)
";
    for function in ["total", "scaled", "has_big", "all_small"] {
        let text = printed(source, function).unwrap();
        assert!(!text.contains("list.append"), "{text}");
        assert!(!text.contains("[]"), "{text}");
    }
}

#[test]
fn a_generator_passed_around_is_a_state_machine() {
    let source = "def upto(limit: int) -> Iterator[int]:
    mut n = 0
    while n < limit:
        yield n
        n += 1

def total(numbers: Iterator[int]) -> int:
    mut so_far = 0
    for n in numbers:
        so_far += n
    so_far

def main_total() -> int:
    total(upto(3))
";
    let made = printed(source, "upto").unwrap();
    assert!(made.contains("= generator upto"), "{made}");
    let resumed = printed(source, "upto.resume").unwrap();
    assert!(resumed.contains("[slot 0] = 1"), "{resumed}");
    let used = printed(source, "total").unwrap();
    assert!(used.contains("= resume"), "{used}");
    assert!(used.contains("this generator has already been used"), "{used}");
}

#[test]
fn a_field_known_not_to_be_none_can_be_read_and_called() {
    let source = "\
struct Inner:
    count: int

    def next(self) -> int:
        self.count + 1

struct Outer:
    inner: Inner?

def read(outer: Outer) -> int:
    if outer.inner is not None:
        return outer.inner.next() + outer.inner.count
    0
";
    let text = printed(source, "read").unwrap();
    assert!(text.contains(".inner"), "{text}");
    assert!(text.contains("Inner.next"), "{text}");
}

/// The locals that are given `true` or `false` somewhere in a function.
fn given_bools(text: &str) -> std::collections::BTreeSet<&str> {
    text.lines()
        .filter_map(|line| line.trim().split_once(" = "))
        .filter(|(_, value)| matches!(*value, "true" | "false"))
        .map(|(local, _)| local)
        .collect()
}

#[test]
fn any_and_all_write_their_answer_to_what_they_give_back() {
    let source = "def has(xs: list[int]) -> bool:
    any(x > 2 for x in xs)

def every(xs: list[int]) -> bool:
    all(x > 2 for x in xs)
";
    for function in ["has", "every"] {
        let text = printed(source, function).unwrap();
        assert_eq!(given_bools(&text).len(), 1, "{text}");
    }
}

#[test]
fn leaving_a_copied_generator_early_writes_one_flag() {
    let source = "def three() -> Iterator[int]:
    yield 1
    yield 2
    yield 3

def first_big() -> int:
    mut found = 0
    for n in three():
        found = n
        if n > 1:
            break
    found
";
    let text = printed(source, "first_big").unwrap();
    assert_eq!(given_bools(&text).len(), 1, "{text}");
}

#[test]
fn a_loop_over_text_makes_no_list() {
    let source = "def spaces(text: str) -> int:
    mut count = 0
    for c in text:
        if c == \" \":
            count += 1
    count
";
    let text = printed(source, "spaces").unwrap();
    assert!(!text.contains("str.chars"), "{text}");
    assert!(text.contains("str.char_at_byte"), "{text}");
}

#[test]
fn equality_calls_a_types_own_def_eq() {
    let source = "\
struct Pair:
    a: int
    b: int

    def ==(self, other: Pair) -> bool:
        self.a + self.b == other.a + other.b

def same(one: Pair, two: Pair) -> bool:
    one == two and not (one != two)
";
    let text = printed(source, "same").unwrap();
    assert_eq!(text.matches("Pair.==").count(), 2, "{text}");
}

#[test]
fn equality_through_a_type_with_its_own_def_eq_compares_each_field_or_isnt_lowered_yet() {
    let source = "\
struct Pair:
    a: int

    def ==(self, other: Pair) -> bool:
        True

struct Held:
    pair: Pair

def same(one: Held, two: Held) -> bool:
    one == two

def found(pair: Pair, pairs: list[Pair]) -> bool:
    pair in pairs
";
    let same = printed(source, "same").unwrap();
    assert!(same.contains(".pair") && same.contains("Pair.=="), "{same}");
    let problem = printed(source, "found").unwrap_err();
    assert!(problem.contains("its own `==`"), "{problem}");
}
