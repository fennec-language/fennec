//! Lowering checked programs to the IR: the printed form is pinned for a few functions, and
//! everything lowered from every example has to pass the validator.

use std::fs;
use std::path::Path;

use fennec_check::Type;
use fennec_ir::facts::Facts;
use fennec_ir::{
    Block, Function, Inst, InstKind, Local, LocalId, Operand, Program, Terminator, lower, validate,
};
use fennec_syntax::Span;

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
fn embedded_bytes_are_a_constant_operand_named_by_their_hash() {
    let data = fennec_syntax::Blob::from(vec![1, 2, 3]);
    let mut module = fennec_syntax::parse("def blob() -> bytes:\n    return \"here\"\n").unwrap();
    fennec_syntax::visit::exprs_mut(&mut module, &mut |expr| {
        if matches!(expr.kind, fennec_syntax::ExprKind::Str(_)) {
            expr.kind = fennec_syntax::ExprKind::Bytes(data.clone());
        }
    });
    let checked = fennec_check::check_and_resolve(&mut module).unwrap();
    let program = lower(&module, &checked);
    validate(&program).unwrap();
    let function = program.functions.iter().find(|function| function.name == "blob").unwrap();
    assert!(function.to_string().contains(&format!("bytes(3, {})", data.hash())), "{function}");
}

#[test]
fn operators_and_tail_values() {
    let source = "def lerp(a: float, b: float, t: float) -> float:\n    return a + (b - a) * t\n";
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
    return n
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
    let source = "def pick(a: bool, b: bool) -> int:\n    return 1 if a and b else 2\n";
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
    return items[0]
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
    mut subtotal = 0
    for item in items:
        subtotal += item
    return subtotal + items[1]
";
    assert_eq!(
        printed(source, "total").unwrap(),
        "fn total(_0) -> int
  local _0 limit: int
  local _1: list[int]
  local _2 items: list[int]
  local _3: int
  local _4: int
  local _5 subtotal: int
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
    builtin list.append(ref _2, 3)
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

    def add(ref self, item: int):
        self.items.append(item)

def grow(ref items: list[int]):
    items.append(1)

def fill(ref bag: Bag):
    grow(ref bag.items)
    bag.add(2)
";
    assert_eq!(
        printed(source, "Bag.add").unwrap(),
        "fn Bag.add(ref _0, _1)
  local _0 self: Bag
  local _1 item: int
  local _2: list[int]
  bb0:
    _2 = _0.items
    builtin list.append(ref _2, _1)
    _0.items = _2
    return
"
    );
    assert_eq!(
        printed(source, "fill").unwrap(),
        "fn fill(ref _0)
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
    return text.startswith(\"a\")
";
    let program = lowered(source).unwrap();
    assert_eq!(program.external, Vec::<String>::new());
    assert_eq!(program.not_lowered, []);
    assert!(program.functions.iter().any(|function| function.name == "str.startswith"));
}

#[test]
fn in_and_not_in_ask_the_engines() {
    let source = "def has(items: list[int], text: str) -> bool:
    return 1 in items and \"a\" not in text
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
    return next
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
        return Point(y=0, x=0)

    def sum(self) -> int:
        return self.x + self.y

def total() -> int:
    return Point.origin().sum()
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
    mut subtotal = 0
    for i in range(n, 0, -1):
        if i == 3: continue
        subtotal += i
    return subtotal
";
    assert_eq!(
        printed(source, "total").unwrap(),
        "fn total(_0) -> int
  local _0 n: int
  local _1 subtotal: int
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
            return .Int(value)
        case .Name(text):
            return .Name(text)
        case .Eof:
            return .Negative(0)
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
            return 0
        case _:
            return 1
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
    return text + \"!\"
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
                return .Green
            case .Green:
                return .Red

def x_of(point: Point) -> int:
    match point:
        case Point(x, _):
            return x

def twice(light: Light) -> Light:
    return light.next().next()
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

def load(path: str) -> str fails Denied | Missing:
    if path == \"\":
        fail Missing(path)
    if path == \"root\":
        fail Denied(path)
    return path

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
    builtin error.raised(ref _2)
    _3 = err _2
    return _3
  bb3:
    jump bb1
  bb4:
    _7 = ok _0
    return _7
  bb5:
    _5 = make Denied(_0)
    builtin error.raised(ref _5)
    _6 = err _5
    return _6
  bb6:
    jump bb4
"
    );
}

#[test]
fn question_mark_leaves_with_the_error() {
    let source = format!(
        "{FILES}def twice(path: str) -> str fails Denied | Missing:\n    text = load(path)?\n    return text + text\n"
    );
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
    builtin error.passed(ref _3)
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
fn matching_a_failure_sees_each_error_and_looks_through_ok() {
    let source = format!(
        "{FILES}def show(path: str) -> str:
    text = load(path) except why:
        case Missing(name):
            name
        case _:
            str(why)
    return text
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
    return count + 1
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

def fill(ref slot: Slot, value: int?) -> int:
    slot.held = 1
    slot.held = None
    mut kept: int? = 2
    kept = None
    if kept is None:
        return 3
    return value else 3
";
    let printed = printed(source, "fill").unwrap();
    for line in ["= some 1", "_0.held = none", "= some 2", "_3 = none", "is none", "as some"] {
        assert!(printed.contains(line), "no {line} in {printed}");
    }
}

/// A member going where its union goes is wrapped with its place in the union as its tag; `is`
/// and `match` test the tag, and what the checker narrowed reads the member back out.
#[test]
fn a_union_is_tagged_by_its_member_and_tested_and_read_by_it() {
    let source = "struct Circle:
    radius: float

    def area(self) -> float:
        return self.radius

struct Square:
    side: float

    def area(self) -> float:
        return self.side

def radius(s: Circle | Square) -> float:
    if s is Circle:
        return s.radius
    return 0.0

def named(s: Circle | Square) -> str:
    match s:
        case Circle(radius): return \"circle\"
        case Square(side): return \"square\"

def area(s: Circle | Square) -> float:
    return s.area()

def made() -> Circle | Square:
    return Square(side=1.0)
";
    assert_eq!(lowered(source).unwrap().not_lowered, []);
    let printed = |name: &str| printed(source, name).unwrap();
    let radius = printed("radius");
    for line in ["is member 0", "as member 0"] {
        assert!(radius.contains(line), "no {line} in {radius}");
    }
    let named = printed("named");
    for line in ["is member 0", "as member 0", "is member 1", "as member 1"] {
        assert!(named.contains(line), "no {line} in {named}");
    }
    // A method every member has: the first member is tested, the last needs no test.
    let area = printed("area");
    assert!(area.contains("is member 0") && !area.contains("is member 1"), "{area}");
    assert!(area.contains("call Circle.area") && area.contains("call Square.area"), "{area}");
    let made = printed("made");
    assert!(made.contains("= member 1 "), "{made}");
}

#[test]
fn matching_an_optional_tests_for_none_first() {
    let source = "def name(code: int?) -> str:
    match code:
        case None:
            return \"none\"
        case 1 | 2:
            return \"low\"
        case n:
            return \"other\"
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
        locals: locals.into_iter().map(|ty| Local { name: None, ty, copied: false }).collect(),
        blocks: vec![Block { insts: Vec::new(), end }],
        counts: Vec::new(),
        places: Vec::new(),
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
    let mut locks =
        function(vec![Type::Named("Point".to_owned())], Terminator::Return(Some(Operand::Int(1))));
    let handle = Operand::Local(LocalId(0));
    locks.blocks[0].insts = vec![
        Inst { kind: InstKind::Lock { handle: handle.clone(), write: true }, span: Span::default() },
        Inst { kind: InstKind::Unlock { handle: handle.clone() }, span: Span::default() },
        Inst { kind: InstKind::LockAll { handles: vec![(handle.clone(), false)] }, span: Span::default() },
        Inst { kind: InstKind::UnlockAll { handles: vec![handle] }, span: Span::default() },
    ];
    assert_eq!(
        problems(locks),
        [
            "f: `lock _0 to write` is on something other than a `shared` handle",
            "f: `unlock _0` is on something other than a `shared` handle",
            "f: `lock all _0 to read` is on something other than a `shared` handle",
            "f: `unlock all _0` is on something other than a `shared` handle",
        ]
    );
}

#[test]
fn a_comprehension_is_a_loop_that_fills_a_collection() {
    let source = "def evens(xs: list[int]) -> list[int]:\n    return [x * 2 for x in xs if x > 0]\n";
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
    builtin list.append(ref _1, _9)
    jump bb3
"
    );
}

#[test]
fn dict_and_set_comprehensions() {
    let source = "def f(xs: list[int]) -> dict[int, str]:\n    return {x: str(x) for x in {x for x in xs}}\n";
    let text = printed(source, "f").unwrap();
    for expected in ["= dict {}", "= set {}", "builtin set.add(ref", "builtin list(", "] = "] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn tuples_unpack_in_assignments_and_loops() {
    let source = "def f(pairs: dict[str, int]) -> int:
    mut total = 0
    for key, value in pairs.items():
        total += value + len(key)
    mut a = total
    mut b = 1
    a, b = (b, a)
    return a + (a, b)[1]
";
    let text = printed(source, "f").unwrap();
    for expected in ["builtin dict.items(", "key: str", "= tuple (", ".0", ".1"] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn dicts_and_slices_read_and_change_items() {
    let source = "def f(xs: list[int], d: dict[str, int]) -> list[int]:
    mut seen = d
    seen[\"a\"] = 1
    if \"b\" in seen:
        return xs[1:]
    return xs[:2]
";
    let text = printed(source, "f").unwrap();
    for expected in ["[\"a\"] = ", "builtin dict.contains(", "= _0[1:]", "= _0[:2]"] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn a_types_own_operators_are_calls() {
    let source = "struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        return Money(self.cents + other.cents)

    def <(self, other: Money) -> bool:
        return self.cents < other.cents

def f(ref a: Money, b: Money) -> bool:
    a += b
    return a >= b
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(ref _0, _1) -> bool
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
    return a == b and c != [1] and d == 3 and c < c
";
    let text = printed(source, "f").unwrap();
    for expected in ["= _0 == _1", "!= ", "= some 3", " < _2"] {
        assert!(text.contains(expected), "no `{expected}` in {text}");
    }
}

#[test]
fn named_arguments_land_in_their_parameters_and_run_in_order() {
    let source = "def g(a: int, b: str) -> int:
    return a + len(b)

def h() -> int:
    return 1

def f() -> int:
    return g(b=str(h()), a=h())
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
    return P(1)
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("= some 3"), "{text}");
    assert!(text.contains("= make P(1, 2.5, _0)"), "{text}");
}

/// Each argument a call leaves out is written in with its own type: a `Duration` and an `int`
/// left out of one call don't take each other's.
/// A call that leaves out an argument with a default, as the standard library's own code runs,
/// unrewritten by the checker: the call takes the default where it's lowered.
#[test]
fn a_call_left_unrewritten_takes_its_defaults() {
    let source = "def added(n: int, more = 1) -> int:\n    return n + more\n\ndef main(stdout: Stdout):\n    stdout.print(added(2))\n";
    let mut module = fennec_syntax::parse(source).unwrap();
    let checked = fennec_check::check_and_resolve(&mut module).unwrap();
    let program = lower(&module, &checked);
    assert!(program.not_lowered.is_empty(), "{:?}", program.not_lowered);
    let main = program.functions.iter().find(|function| function.name == "main").unwrap().to_string();
    assert!(main.contains("call added(2, 1)"), "{main}");
}

#[test]
fn arguments_left_out_get_their_defaults_with_their_types() {
    let source = "from std.time import Duration

def wait(label: str, timeout=30s, tries=3) -> int:
    return len(label) + tries

def f() -> int:
    return wait(\"a\") + wait(\"b\", tries=1)
";
    let program = lowered(source).unwrap();
    assert_eq!(program.not_lowered, [], "{program}");
    let text = printed(source, "f").unwrap();
    assert!(text.contains("call wait(\"a\", "), "{text}");
}

#[test]
fn literal_constants_are_written_out_and_others_are_functions() {
    let source = "LIMIT = -2
WORDS = [\"a\", \"b\"]
DOUBLE = LIMIT * 2

def f() -> int:
    return len(WORDS) + LIMIT + DOUBLE
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
    return sqrt(x) * PI
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
    return total
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("builtin range(0, _0, 2)"), "{text}");
}

#[test]
fn for_mut_writes_each_item_back_however_the_loop_is_left() {
    let source = "def f(ref xs: list[int]) -> int:
    for ref x in xs:
        x += 1
        if x > 3:
            break
        if x < 0:
            return x
    return 0
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(ref _0) -> int
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

def f(ref ps: list[P], ref grid: list[list[int]]):
    ps[0].x += 1
    grid[1][2] = 3
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(ref _0, ref _1)
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
        return self.cells[at]

    def []=(ref self, at: int, value: int):
        self.cells[at] = value

struct Holder:
    grid: Grid

def f(ref holder: Holder) -> int:
    holder.grid[1] = 2
    return holder.grid[0]
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(ref _0) -> int
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
    let source = "def bump(ref n: int):
    n += 1

def f(ref xs: list[int], ref lists: list[list[int]]):
    bump(ref xs[0])
    lists[1].append(2)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(ref _0, ref _1)
  local _0 xs: list[int]
  local _1 lists: list[list[int]]
  local _2: int
  local _3: list[int]
  bb0:
    _2 = _0[0]
    call bump(_2)
    _0[0] = _2
    _3 = _1[1]
    builtin list.append(ref _3, 2)
    _1[1] = _3
    return
"
    );
}

#[test]
fn falling_off_the_end_of_a_function_that_can_fail_succeeds() {
    let source = "error Bad:
    code: int

def f(x: int) fails Bad:
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
        return self.all[at]

def f(names: Names) -> str:
    return names[0]
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("call Names.[](_0, 0)"), "{text}");
}

#[test]
fn an_arrow_function_is_lifted_with_what_it_captures() {
    let source = "def apply(value: int, change: (int) -> int) -> int:
    return change(value)

def f(x: int) -> int:
    offset = x + 1
    return apply(x, (n) => n + offset)
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
    _3 = closure f.arrow@139[_2]
    _4 = call apply(_0, _3)
    return _4
fn f.arrow@139(_0, _1) -> int
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
        return down(n - 1)
    return down(10)
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
    return n * 2

def f() -> int:
    g = double
    return g(2)
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
    return sorted(xs, key=(n) => -n)
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f(_0) -> list[int]
  local _0 xs: list[int]
  local _1: (int) -> int
  local _2: list[int]
  bb0:
    _1 = closure f.arrow@61[]
    _2 = builtin sorted.key(_0, _1)
    return _2
"
    );
}

#[test]
fn a_tuple_of_one_type_and_the_arguments_index_by_a_value() {
    let source = "def f(pair: (int, int), at: int) -> int:
    return pair[at]

def g(args: Args, at: int) -> str:
    return args[at]

def h(data: bytes, at: int) -> int:
    return data[at]
";
    assert!(printed(source, "f").unwrap().contains("= _0[_1]"));
    assert!(printed(source, "g").unwrap().contains("= _0[_1]"));
    assert!(printed(source, "h").unwrap().contains("= _0[_1]"));
}

#[test]
fn a_loop_over_a_type_goes_through_what_its_iter_gives() {
    let source = "struct Bag:
    all: list[int]

    def iter(self) -> Iterator[int]:
        for one in self.all:
            yield one

def f(bag: Bag) -> list[int]:
    return [one for one in bag]
";
    // Its body copied into the loop, with no generator.
    let text = printed(source, "f").unwrap();
    assert!(!text.contains("Bag.iter") && !text.contains("resume"), "{text}");
    // One that can't be copied is called, and the loop resumes what it gives.
    let leaving = source.replace("            yield one", "            yield one\n        return");
    let text = printed(&leaving, "f").unwrap();
    assert!(text.contains("= call Bag.iter(_0)"), "{text}");
}

#[test]
fn a_tuple_pattern_tests_each_part() {
    let source = "def f(pair: (int, str)) -> int:
    match pair:
        case (0, _):
            return 1
        case (x, _):
            return x
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

/// One case on the line, `except InvalidValue(): 8080`, covers every failure and binds nothing, so
/// the failure isn't taken out to match, as it is for `except why:`.
#[test]
fn one_case_that_binds_nothing_takes_nothing_out() -> Result<(), String> {
    let program = |handler: &str| {
        format!(
            "def port_of(text: str) -> int:\n    return int.parse(text) {handler}\n\ndef main(stdout: Stdout):\n    stdout.print(port_of(\"80\"))\n    return\n"
        )
    };
    let one_case = printed(&program("except InvalidValue(): 8080"), "port_of")?;
    assert!(one_case.contains("8080"), "{one_case}");
    // One that binds what failed takes it out, as `except why:` does.
    let named = printed(&program("except why: 8080 if why.message == \"\" else 1"), "port_of")?;
    assert!(named.contains("err"), "{named}");
    assert_ne!(one_case, named);
    Ok(())
}

#[test]
fn an_else_chain_gives_the_default_for_none() {
    let source = "struct P:
    level: int

def f(p: P?) -> int:
    return p.level else 0
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

def f(n: int) -> Score fails InvalidValue:
    return Score(n)?

def g() -> Score:
    return Score(1)
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
    builtin error.passed(ref _7)
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

def f(text: str) -> list[int] fails ParseError:
    a = geometry.sqrt(16.0)
    b = data.dumps(a)
    t = toml_loads[dict[str, int]](text)?
    return data.loads[list[int]](b)? + t.values()
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
    builtin error.passed(ref _7)
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
    builtin error.passed(ref _13)
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
    let source = "from std.collections import Table

struct Enemy:
    health: int

    def hit(ref self):
        self.health -= 1

def f() -> int:
    mut map = Table[Enemy]()
    one = map.add(Enemy(2))
    map[one] = Enemy(3)
    _ = map.modify(one, e => e.hit())
    for ref enemy in map:
        enemy.hit()
    return map[one].health
";
    assert_eq!(
        printed(source, "f").unwrap(),
        "fn f() -> int
  local _0: Table[Enemy]
  local _1 map: Table[Enemy]
  local _2: Enemy
  local _3: Id[Enemy]
  local _4 one: Id[Enemy]
  local _5: Enemy
  local _6: (Enemy) -> nothing
  local _7: bool
  local _8 _'0: bool
  local _9: int
  local _10: int
  local _11: bool
  local _12: bool
  local _13: Id[Enemy]
  local _14 enemy: Enemy
  local _15: Enemy
  local _16: int
  bb0:
    _0 = builtin Table()
    _1 = _0
    _2 = make Enemy(2)
    _3 = builtin Table.add(ref _1, _2)
    _4 = _3
    _5 = make Enemy(3)
    _1[_4] = _5
    _6 = closure f.arrow@236[]
    _7 = builtin Table.modify(ref _1, _4, _6)
    _8 = _7
    _9 = builtin Table.slots(_1)
    _10 = 0
    jump bb1
  bb1:
    _11 = _10 < _9
    branch _11 bb2 bb4
  bb2:
    _12 = builtin Table.slot_live(_1, _10)
    branch _12 bb3 bb5
  bb3:
    _13 = builtin Table.slot_id(_1, _10)
    _14 = builtin Table.at_slot(_1, _10)
    call Enemy.hit(_14)
    jump bb6
  bb4:
    _15 = _1[_4]
    _16 = _15.health
    return _16
  bb5:
    _10 = _10 + 1
    jump bb1
  bb6:
    _1[_13] = _14
    _10 = _10 + 1
    jump bb1
"
    );
}

#[test]
fn an_argument_is_copied_only_when_a_later_one_changes_it() {
    let lowered = lowered(
        "def show(a: list[int], b: int) -> int:
    return b

def kept(ref xs: list[int], ys: list[int]) -> int:
    return show(xs, len(ys)) + show(ys, len(xs))

def changed(ref xs: list[int]) -> int:
    return show(xs, xs.pop(0))
",
    )
    .unwrap();
    let copies = |name: &str| {
        let function = lowered.functions.iter().find(|function| function.name == name);
        function.map_or(0, |function| {
            let insts = function.blocks.iter().flat_map(|block| &block.insts);
            insts.filter(|inst| matches!(inst.kind, InstKind::Copy { .. })).count()
        })
    };
    assert_eq!(copies("kept"), 0);
    assert_eq!(copies("changed"), 1);
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
    builtin assert.sides(_0, 0)
    stop \"this assertion failed\"
"
    );
}

#[test]
fn an_else_chain_call_runs_only_on_a_value() {
    let source = "struct P:
    level: int

    def up(self) -> int:
        return self.level + 1

def f(p: P?) -> int:
    return p.up() else 0
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
fn copy_with_fields_copies_and_changes_them() {
    let source = "struct P:
    x: int
    y: int? = None

def f(p: P) -> P:
    return copy(p, y=1)
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

    def up(ref self):
        self.x += 1

def f(p: P?) -> P?:
    mut q = p
    if q is None:
        return None
    q.up()
    return q
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
    return json.dumps(xs, indent=2)
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
    return (Body().position + Vec3(x=3.0)).length()
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
    return items[0]

def f() -> str:
    a = first([1])
    b = first([\"x\"])
    c = first([2])
    return f\"{a} {b} {c}\"
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
        return self.side * self.side

def twice[T: Shape](shape: T) -> float:
    return shape.area() * 2.0

def f() -> float:
    return twice(Square(1.0))
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
        return self.held

def f() -> int:
    a = Box(1)
    b = Box(\"x\")
    return a.get() + len(b.get())
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
        "StructDef { name: \"Box[int]\", fields: [(\"held\", Int), (\"count\", Int)], shared: false }, \
         StructDef { name: \"Box[str]\", fields: [(\"held\", Str), (\"count\", Int)], shared: false }"
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
    let source = "def f(ref xs: list[int]) -> int:
    xs.reverse()
    return xs.count(1)
";
    let program = lowered(source).unwrap();
    let names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    assert_eq!(names, ["f", "list.count[int]", "list.reverse[int]"]);
}

#[test]
fn a_nested_function_in_a_generic_uses_what_its_types_stand_for() {
    let source = "def f[T](x: T) -> list[T]:
    def again() -> list[T]:
        return [x, x]
    return again()

def h() -> list[int]:
    return f(1)
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
    return grow([x], n - 1)

def f() -> int:
    return grow(1, 3)
";
    let program = lowered(source).unwrap();
    assert_ne!(program.not_lowered, []);
    assert!(program.functions.len() <= 2001, "{}", program.functions.len());
}

#[test]
fn lowering_some_starts_only_from_what_it_picks() {
    let source = "LIST = [1]

def a() -> int:
    return b()

def b() -> int:
    return LIST[0]

def c() -> int:
    return 1
";
    let mut module = fennec_syntax::parse(source).unwrap();
    let checked = fennec_check::check_and_resolve(&mut module).unwrap();
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
    return n - 1
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
    let source = "def bump(ref x: int):
    x += 1

def f(n: int) -> int:
    mut total = 0
    bump(ref total)
    once = 5
    for i in range(3, 9):
        total += i + once
    return total
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

def f(ref pair: Pair):
    pair.low = 1
";
    let text = printed(source, "f").unwrap();
    assert!(text.contains("`Pair.low`"), "{text}");
    assert!(!text.contains("`Pair.high`"), "{text}");
}

const ACCOUNT: &str = "struct Account:
    total = 0
    log = list[int]()

    def add(ref self, amount: int):
        self.total += amount
        self.log.append(amount)
        self.note()

    def note(self):
        pass

    def balance(self) -> int:
        return self.total


def pay(account: shared Account, amount: int):
    lock account as ref held:
        held.add(amount)


def main(stdout: Stdout):
    shared account = Account()
    same = account
    pay(account, 5)
    lock same as ref held:
        held.add(10)
    lock account as held:
        stdout.print(held.balance(), held.total, held)
";

#[test]
fn a_shared_value_is_locked_for_each_lock_block() {
    let program = lowered(ACCOUNT).unwrap();
    let shared = |name: &str| program.structs.iter().find(|def| def.name == name).map(|def| def.shared);
    // `shared Account` is a hidden `shared` type holding a plain `Account`.
    assert_eq!(shared("Account"), Some(false));
    assert_eq!(shared("Shared__Account"), Some(true));
    // `Account`'s own methods take no lock: they're called on what a `lock` holds.
    assert_eq!(
        printed(ACCOUNT, "Account.add").unwrap(),
        "fn Account.add(ref _0, _1)
  local _0 self: Account
  local _1 amount: int
  local _2: int
  local _3: int
  local _4: list[int]
  bb0:
    _2 = _0.total
    _3 = _2 + _1
    _0.total = _3
    _4 = _0.log
    builtin list.append(ref _4, _1)
    _0.log = _4
    call Account.note(_0)
    return
"
    );
    // Each `lock` locks the value for its block, to write with `ref` and to read without, and
    // what it holds is taken out, changed and put back while it's held.
    assert_eq!(
        printed(ACCOUNT, "main").unwrap(),
        "fn main(_0)
  local _0 stdout: Stdout
  local _1: list[int]
  local _2: Account
  local _3: Shared__Account
  local _4 account: Shared__Account
  local _5 same: Shared__Account
  local _6 held__lock: Shared__Account
  local _7: Account
  local _8 held__lock: Shared__Account
  local _9: Account
  local _10: int
  local _11: Account
  local _12: int
  local _13: Account
  bb0:
    _1 = []
    _2 = make Account(0, _1)
    _3 = make Shared__Account(_2)
    _4 = _3
    _5 = _4
    call pay(_4, 5)
    _6 = _5
    lock _6 to write
    _7 = _6.value
    call Account.add(_7, 10)
    _6.value = _7
    unlock _6
    _8 = _4
    lock _8 to read
    _9 = _8.value
    _10 = call Account.balance(_9)
    _11 = _8.value
    _12 = _11.total
    _13 = _8.value
    builtin Stdout.print(_0, _10, _12, _13)
    unlock _8
    return
"
    );
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

/// Examples that use what lowering doesn't take yet.
const NOT_LOWERED_YET: &[&str] = &[];

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
        if NOT_LOWERED_YET.contains(&file.as_str()) {
            assert_ne!(program.not_lowered, [], "{file} lowers now: take it off `NOT_LOWERED_YET`");
            continue;
        }
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
    return sum_so_far

def doubled(limit: int) -> list[int]:
    return [n for n in (x * 2 for x in upto(limit)) if n > 2]
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
    return sum(n * 2 for n in upto(limit) if n > 1)

def scaled(xs: list[float]) -> float:
    return sum(x * 2.0 for x in xs)

def has_big(limit: int) -> bool:
    return any(n > 2 for n in upto(limit))

def all_small(xs: list[int]) -> bool:
    return all(x < 2 for x in xs)
";
    for function in ["total", "scaled", "has_big", "all_small"] {
        let text = printed(source, function).unwrap();
        assert!(!text.contains("list.append"), "{text}");
        assert!(!text.contains("[]"), "{text}");
    }
}

/// A `with` source whose cleanup, written after its `lend`, says when it runs.
const DOOR: &str = "struct Door:
    name: str

def closed(name: str):
    pass

def opened(name: str) -> with Door:
    door = Door(name=name)
    lend door
    closed(door.name)
";

/// `f` below, lowered: each way out of the block reaches `call closed` in `bb6`.
const LEFT_EVERY_WAY: &str = "fn f(_0) -> int
  local _0 n: int
  local _1: bool
  local _2 name__lend1: str
  local _3 exit__lend1: int
  local _4 returned__lend1: int?
  local _5: Door
  local _6 door__lend1: Door
  local _7: bool
  local _8: bool
  local _9: str
  local _10: int
  local _11: int?
  local _12: str
  local _13: bool
  local _14: bool
  local _15: bool
  local _16: int
  local _17: bool
  local _18 name__lend2: str
  local _19: Door
  local _20 door__lend2: Door
  local _21: str
  bb0:
    jump bb1
  bb1:
    _1 = _0 > 0
    branch _1 bb2 bb3
  bb2:
    _2 = \"a\"
    _3 = 0
    _4 = none
    _5 = make Door(_2)
    _6 = _5
    jump bb4
  bb3:
    _18 = \"b\"
    _19 = make Door(_18)
    _20 = _19
    _21 = _20.name
    call closed(_21)
    return 0
  bb4:
    jump bb5
  bb5:
    _7 = _0 == 1
    branch _7 bb8 bb9
  bb6:
    _12 = _6.name
    call closed(_12)
    _13 = _3 == 1
    branch _13 bb14 bb15
  bb7:
    _8 = _0 == 2
    branch _8 bb11 bb12
  bb8:
    _3 = 2
    jump bb6
  bb9:
    jump bb7
  bb10:
    jump bb6
  bb11:
    _9 = _6.name
    _10 = builtin str.len(_9)
    _11 = some _10
    _4 = _11
    _3 = 1
    jump bb6
  bb12:
    jump bb10
  bb13:
    _17 = _3 == 2
    branch _17 bb20 bb21
  bb14:
    _14 = _4 is none
    _15 = not _14
    branch _15 bb17 bb18
  bb15:
    jump bb13
  bb16:
    jump bb13
  bb17:
    _16 = _4 as some
    return _16
  bb18:
    jump bb16
  bb19:
    jump bb1
  bb20:
    jump bb3
  bb21:
    jump bb19
";

#[test]
fn a_with_sources_cleanup_runs_however_its_block_is_left() {
    // `break`, `return` and the block's end all run `closed` where the source's cleanup is
    // written in, and then leave the way they asked to.
    let source = format!(
        "{DOOR}
def f(n: int) -> int:
    while n > 0:
        with opened(\"a\") as door:
            if n == 1:
                break
            if n == 2:
                return len(door.name)
    with opened(\"b\") as other:
        pass
    return 0
"
    );
    assert_eq!(printed(&source, "f").unwrap(), LEFT_EVERY_WAY);
}

#[test]
fn a_loop_over_a_generator_with_a_source_or_match_still_lowers() {
    let source = format!(
        "{DOOR}
def held(name: str) -> Iterator[int]:
    with opened(name) as door:
        yield 1
    match 2:
        case 2:
            yield 2
        case _:
            return

def f() -> int:
    return sum(n for n in held(\"a\"))
"
    );
    // What the source lent is kept across the `yield`, and its cleanup is written in once, where
    // the block's ways out meet.
    let resume = printed(&source, "held.resume").unwrap();
    assert_eq!(resume.matches("call closed").count(), 1, "{resume}");
    // Let go inside the block, it goes on once more from where it stopped, told to leave, and
    // leaves through the source's cleanup.
    let program = lowered(&source).unwrap();
    let generators: Vec<_> =
        program.generators.iter().map(|def| (&*def.name, def.abandon.as_deref())).collect();
    assert_eq!(generators, [("held", Some("held.abandon"))]);
    let abandon = printed(&source, "held.abandon").unwrap();
    assert!(abandon.contains("[slot 0] = -4") && abandon.contains("resume _0"), "{abandon}");
    assert!(resume.contains(" == -4"), "{resume}");
    assert!(!printed(&source, "f").unwrap().contains("list.append"));
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
    return so_far

def main_total() -> int:
    return total(upto(3))
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
        return self.count + 1

struct Outer:
    inner: Inner?

def read(outer: Outer) -> int:
    if outer.inner is not None:
        return outer.inner.next() + outer.inner.count
    return 0
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
    return any(x > 2 for x in xs)

def every(xs: list[int]) -> bool:
    return all(x > 2 for x in xs)
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
    return found
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
    return count
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
        return self.a + self.b == other.a + other.b

def same(one: Pair, two: Pair) -> bool:
    return one == two and not (one != two)
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
        return True

struct Held:
    pair: Pair

def same(one: Held, two: Held) -> bool:
    return one == two

def found(pair: Pair, pairs: list[Pair]) -> bool:
    return pair in pairs
";
    let same = printed(source, "same").unwrap();
    assert!(same.contains(".pair") && same.contains("Pair.=="), "{same}");
    let problem = printed(source, "found").unwrap_err();
    assert!(problem.contains("its own `==`"), "{problem}");
}

/// Every lowered function whose name starts with `name`, printed.
fn printed_copies(source: &str, name: &str) -> Result<String, String> {
    let program = lowered(source)?;
    let found: Vec<String> = program
        .functions
        .iter()
        .filter(|function| function.name.starts_with(name))
        .map(ToString::to_string)
        .collect();
    if found.is_empty() {
        return Err(format!("no {name}: {:?}", program.not_lowered));
    }
    Ok(found.join("\n"))
}

#[test]
fn a_loop_over_what_a_yield_method_gives_is_a_plain_loop() {
    let methods = "
    def [](self, at: int) -> Iterator[T]:
        for item in self.items[self.starts[at]:self.starts[at + 1]]:
            yield item

    def all(self) -> Iterator[T]:
        for item in self.items:
            yield item

    def iter(self) -> Iterator[T]:
        for item in self.items:
            yield item
";
    let total = "
def total(packed: Packed[int], at: int) -> int:
    mut sum_so_far = 0
    for n in packed[at]:
        sum_so_far += n
    for n in packed.all():
        sum_so_far += n
    for n in packed:
        sum_so_far += n
    return sum_so_far

def main(stdout: Stdout):
    stdout.print(total(Packed(items=[1, 2, 3], starts=[0, 2, 3]), 1))
";
    let generic = format!("struct Packed[T]:\n    items: list[T]\n    starts: list[int]\n{methods}{total}");
    let plain = generic
        .replace("Packed[T]", "Packed")
        .replace("Iterator[T]", "Iterator[int]")
        .replace("list[T]", "list[int]")
        .replace("Packed[int]", "Packed");
    for source in [plain, generic] {
        let text = printed_copies(&source, "total").unwrap();
        assert!(!text.contains("generator") && !text.contains("resume"), "{text}");
        assert!(!text.contains("call "), "{text}");
    }
}

#[test]
fn a_yield_method_unpacking_a_tuple_fuses() {
    let source = "struct Pairs:
    items: list[(int, int)]

    def iter(self) -> Iterator[int]:
        mut trail = self.items
        while len(trail) > 0:
            first, second = trail[-1]
            _ = trail.pop()
            yield first + second

def total(pairs: Pairs) -> int:
    mut sum_so_far = 0
    for n in pairs:
        sum_so_far += n
    return sum_so_far

def main(stdout: Stdout):
    stdout.print(total(Pairs(items=[(1, 2), (3, 4)])))
";
    let text = printed_copies(source, "total").unwrap();
    assert!(!text.contains("generator") && !text.contains("resume"), "{text}");
}

#[test]
fn a_buffer_is_made_filled_and_read_through_its_own_builtins() {
    let source = "def main(stdout: Stdout):\n    mut b = Buffer[int](capacity=2)\n    b.extend(Buffer[int]())\n    b.insert(0, 1)\n    stdout.print(b.remove(0))\n    b.push(3)\n    b[0] = 4\n    b.reserve(8)\n    b.truncate(1)\n    stdout.print(b[0], len(b), b.capacity(), b.pop())\n";
    let text = printed(source, "main").unwrap();
    assert!(text.contains("builtin Buffer.new.capacity(2)"), "{text}");
    for name in
        ["Buffer.push", "Buffer.reserve", "Buffer.truncate", "Buffer.len", "Buffer.capacity", "Buffer.pop"]
    {
        assert!(text.contains(name), "{name} in\n{text}");
    }
}

const TRANSFER: &str = "struct Account:
    total = 0

    def add(ref self, amount: int):
        self.total += amount

def transfer(a: shared Account, b: shared Account, amount: int) -> bool:
    lock a, b as ref x, ref y:
        if x.total < amount:
            return False
        x.total -= amount
        y.add(amount)
    return True

def peek(a: shared Account) -> int:
    lock a as seen:
        return seen.total
";

#[test]
fn a_lock_locks_its_values_for_the_block() {
    // Several values lock together, in an order the backend fixes, and unlock on every way out.
    // Through the names the block gives them, nothing locks again.
    assert_eq!(
        printed(TRANSFER, "transfer").unwrap(),
        "fn transfer(_0, _1, _2) -> bool
  local _0 a: Shared__Account
  local _1 b: Shared__Account
  local _2 amount: int
  local _3 x__lock: Shared__Account
  local _4 y__lock: Shared__Account
  local _5: Account
  local _6: int
  local _7: bool
  local _8: Account
  local _9: int
  local _10: int
  local _11: Account
  bb0:
    _3 = _0
    _4 = _1
    lock all _3 to write, _4 to write
    _5 = _3.value
    _6 = _5.total
    _7 = _6 < _2
    branch _7 bb2 bb3
  bb1:
    _8 = _3.value
    _9 = _8.total
    _10 = _9 - _2
    _3.value.total = _10
    _11 = _4.value
    call Account.add(_11, _2)
    _4.value = _11
    unlock all _3, _4
    return true
  bb2:
    unlock all _3, _4
    return false
  bb3:
    jump bb1
"
    );
    // One value takes its own lock, to read when its name isn't `ref`.
    assert_eq!(
        printed(TRANSFER, "peek").unwrap(),
        "fn peek(_0) -> int
  local _0 a: Shared__Account
  local _1 seen__lock: Shared__Account
  local _2: Account
  local _3: int
  local _4: int
  bb0:
    _1 = _0
    lock _1 to read
    _2 = _1.value
    _3 = _2.total
    _4 = _3
    unlock _1
    return _4
"
    );
}

const DIAL_ERROR: &str = "error DialError:
    NotFound(host: str)
    Refused(host: str, port: int)
    Closed

def connect(host: str, port: int) -> int fails DialError:
    if host == \"\":
        fail .NotFound(host)
    if port == 0:
        fail .Closed
    return port

def described(port: int) -> str:
    got = connect(\"db\", port) except why:
        case .Refused(host, port):
            return f\"refused {host}:{port}\"
        case _:
            return f\"failed: {why}\"
    return f\"port {got}\"
";

#[test]
fn an_error_with_cases_is_held_like_an_enum() {
    let program = lowered(DIAL_ERROR).unwrap();
    assert!(program.not_lowered.is_empty(), "{:?}", program.not_lowered);
    let held = program.enums.iter().find(|def| def.name == "DialError");
    assert_eq!(held.map(|def| def.cases.len()), Some(3));
    // It says it's an error, which carries notes, as a plain enum doesn't.
    assert_eq!(held.map(|def| def.error), Some(true));
    let plain =
        lowered("enum Kind:\n    One\n    Two\n\ndef main(stdout: Stdout):\n    stdout.print(Kind.One)\n")
            .unwrap();
    let kind = plain.enums.iter().find(|def| def.name == "Kind").map(|def| def.error);
    assert_eq!(kind, Some(false));
    assert!(!program.errors.iter().any(|def| def.name == "DialError"));
    assert_eq!(
        printed(DIAL_ERROR, "connect").unwrap(),
        "fn connect(_0, _1) -> result of int
  local _0 host: str
  local _1 port: int
  local _2: bool
  local _3: DialError
  local _4: result of int
  local _5: bool
  local _6: DialError
  local _7: result of int
  local _8: result of int
  bb0:
    _2 = _0 == \"\"
    branch _2 bb2 bb3
  bb1:
    _5 = _1 == 0
    branch _5 bb5 bb6
  bb2:
    _3 = make DialError.NotFound(_0)
    builtin error.raised(ref _3)
    _4 = err _3
    return _4
  bb3:
    jump bb1
  bb4:
    _8 = ok _1
    return _8
  bb5:
    _6 = make DialError.Closed()
    builtin error.raised(ref _6)
    _7 = err _6
    return _7
  bb6:
    jump bb4
"
    );
}

/// `item in value` calls the value's own `def in`, with the value first, and `not in` flips it.
#[test]
fn in_calls_a_types_own_in() {
    let source = "struct Span:\n    low: int\n    high: int\n\n    def in(self, n: int) -> bool:\n        return self.low <= n < self.high\n\ndef inside(span: Span, n: int) -> bool:\n    return n in span\n\ndef outside(span: Span, n: int) -> bool:\n    return n not in span\n";
    assert_eq!(
        printed(source, "inside").unwrap(),
        "fn inside(_0, _1) -> bool
  local _0 span: Span
  local _1 n: int
  local _2: bool
  bb0:
    _2 = call Span.in(_0, _1)
    return _2
"
    );
    assert_eq!(
        printed(source, "outside").unwrap(),
        "fn outside(_0, _1) -> bool
  local _0 span: Span
  local _1 n: int
  local _2: bool
  local _3: bool
  bb0:
    _2 = call Span.in(_0, _1)
    _3 = not _2
    return _3
"
    );
}

/// Printing a value whose type has its own `def str` calls it on a fresh `StrWriter` and uses
/// what it wrote, so native code never prints such a value from its fields.
#[test]
fn a_type_with_its_own_str_prints_through_it() {
    let source = "struct Point:\n    x: int\n\n    def str(self, ref out: StrWriter):\n        out.value(self.x)\n\ndef label(p: Point) -> str:\n    return f\"at {p}\"\n";
    assert_eq!(
        printed(source, "label").unwrap(),
        r#"fn label(_0) -> str
  local _0 p: Point
  local _1: list[str]
  local _2: StrWriter
  local _3: str
  local _4: str
  bb0:
    _1 = []
    _2 = make StrWriter(_1)
    call Point.str(_0, _2)
    _3 = call StrWriter.text(_2)
    _4 = format "at " {_3}
    return _4
"#
    );
}

/// Two `with` blocks over one generic source, of different types, in one function: each written-in
/// copy of the source's text has types of its own, so neither takes the other's.
#[test]
fn two_channels_of_different_types_in_one_function_lower() {
    let source = "from std.concurrent import Closed, Timeout, channel\n\n\
        def main(stdout: Stdout) fails Closed | Timeout:\n    \
        with channel[int](size=4) as numbers:\n        numbers.send(1)?\n        stdout.print(numbers.receive() else fail Closed())\n    \
        with channel[list[float]](size=1) as lists:\n        lists.send([1.5])?\n        stdout.print(lists.receive() else fail Closed())\n";
    let program = lowered(source).unwrap();
    assert!(program.not_lowered.is_empty(), "{:?}", program.not_lowered);
}

/// `with launch(...)`, written in as a source, lowers: its task as a closure given to
/// `intrinsics.launch`, and its end through `intrinsics.launch_end`.
#[test]
fn with_launch_lowers() {
    let source = "from std.concurrent import Closed, Crashed, Inbox, Timeout\n\nenum Sound:\n    Play(name: str, volume: float)\n    StopAll\n\nerror Muted:\n    why: str\n\ndef audio(inbox: Inbox[Sound], stdout: Stdout) fails Muted:\n    for sound in inbox:\n        match sound:\n            case .Play(name, volume):\n                if volume > 1.0: fail Muted(why=\"too loud\")\n                stdout.print(f\"play {name}\")\n            case .StopAll:\n                stdout.print(\"stop\")\n\ndef tick(stdout: Stdout):\n    stdout.print(\"tick\")\n\ndef main(stdout: Stdout) fails Closed | Crashed | Muted | Timeout:\n    with launch((inbox) => audio(inbox, stdout)?, stop_after=5s, inbox_size=4)? as sounds:\n        sounds.send(.Play(name=\"jump\", volume=0.8))?\n        sounds.send(.StopAll)?\n    with launch(() => tick(stdout))?:\n        stdout.print(\"ticking\")\n";
    let program = lowered(source).unwrap();
    assert!(program.not_lowered.is_empty(), "{:?}", program.not_lowered);
}

/// A launched task that can fail several ways: `launch_end` is written with their union in its
/// brackets, which reads as one type, and the program checks again and lowers.
#[test]
fn a_task_failing_several_ways_lowers() {
    let source = "from std.concurrent import Channel, Closed, Crashed, Timeout, channel\n\n\
        def tell(replies: Channel[int]) fails Closed | Timeout:\n    replies.send(1)?\n\n\
        def main(stdout: Stdout) fails Closed | Crashed | Timeout:\n    \
        with channel[int](size=4) as replies:\n        with launch(() => tell(replies)?)?:\n            pass\n        \
        stdout.print(replies.receive() else fail Closed())\n";
    let program = lowered(source).unwrap();
    assert!(program.not_lowered.is_empty(), "{:?}", program.not_lowered);
}

#[test]
fn a_result_passed_on_fails_every_way_its_function_can() {
    // `outer` fails with `Worse` itself and passes up `inner`'s failure, which is `Bad`, so it
    // gives back a result that can fail with either.
    let source = "error Bad:\n    why: str\n\nerror Worse:\n    why: str\n\ndef inner(n: int) -> int fails Bad:\n    \
                  if n < 0:\n        fail Bad(why=\"neg\")\n    return n\n\ndef outer(n: int) -> int fails Bad | Worse:\n    if n > 9:\n        \
                  fail Worse(why=\"big\")\n    return inner(n)?\n";
    let program = lowered(source).unwrap();
    assert!(program.not_lowered.is_empty(), "{:?}", program.not_lowered);
    let outer = program.functions.iter().find(|function| function.name == "outer").unwrap();
    let errors = Type::Errors(vec!["Bad".to_owned(), "Worse".to_owned()]);
    assert_eq!(outer.returns, Type::Result(Box::new(Type::Int), Box::new(errors)));
}

/// A `with` source's `else:` runs when its block crashes as well as when it fails: it's a closure
/// marked to run on a crash where the block starts, and unmarked where the block ends, before the
/// cleanup a failure runs, and before the one written in at a `?`. One that would pass a failure
/// up some other way isn't marked.
#[test]
fn a_with_blocks_else_is_marked_to_run_on_a_crash() {
    let source = "error Boom:\n    pass\n\n\
        def risky(n: int) -> int fails Boom:\n    if n > 2:\n        fail Boom()\n    return n\n\n\
        def opened(stdout: Stdout) -> with int fails Boom:\n    lend 7 except:\n        stdout.print(\"cleanup\")\n    stdout.print(\"closed\")\n\n\
        def used(stdout: Stdout) fails Boom:\n    with opened(stdout)? as held:\n        stdout.print(risky(held)?)\n\n\
        def main(stdout: Stdout):\n    used(stdout) except Boom():\n        return\n";
    let function = printed(source, "used").unwrap();
    assert_eq!(function.matches("on crash run ").count(), 1, "{function}");
    assert!(function.matches("on crash no longer run ").count() >= 2, "{function}");
}

/// A standard library function rewritten for a program, as one holding a `with` source is,
/// stands in for the library's own: the check reads it and the IR lowers it.
#[test]
fn a_rewritten_std_function_stands_in_for_the_librarys() {
    let mut module = fennec_syntax::parse(
        "from std import base64\n\ndef main(stdout: Stdout):\n    stdout.print(base64.encode(\"hi\".encode()))\n",
    )
    .unwrap();
    let mut rewritten =
        fennec_syntax::parse("def encode(data: bytes) -> str:\n    return \"rewritten\"\n").unwrap();
    let Some(fennec_syntax::Item::Function(mut def)) = rewritten.items.pop() else { panic!("no function") };
    // Its text is the library's, as a rewritten one's is.
    let moved = |span: Span| Span {
        start: span.start + Span::PRELUDE + (1 << 29),
        end: span.end + Span::PRELUDE + (1 << 29),
    };
    def.span = moved(def.span);
    for param in &mut def.params {
        param.span = moved(param.span);
        if let Some(ty) = &mut param.ty {
            ty.span = moved(ty.span);
        }
    }
    if let Some(ret) = &mut def.ret {
        ret.span = moved(ret.span);
    }
    for stmt in &mut def.body {
        stmt.span = moved(stmt.span);
    }
    fennec_syntax::visit::in_function(&mut def, &mut |expr| expr.span = moved(expr.span));
    def.name = "base64.encode".to_owned();
    module.std_rewritten.push(("base64.encode".to_owned(), def));
    let checked = fennec_check::check_and_resolve(&mut module).unwrap();
    let program = lower(&module, &checked);
    assert!(program.not_lowered.is_empty(), "{:?}", program.not_lowered);
    let encode =
        program.functions.iter().find(|function| function.name == "base64.encode").map(ToString::to_string);
    assert!(encode.as_deref().is_some_and(|text| text.contains("\"rewritten\"")), "{encode:?}");
}
