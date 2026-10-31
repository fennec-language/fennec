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
    x: int = 0

struct Checked:
    x: int where x > 0

def make() -> Point:
    Point()

def checked() -> Checked:
    Checked(1)

def pair() -> (int, int):
    (1, 2)

def squares(n: int) -> list[int]:
    [i * i for i in range(n)]

def count(words: dict[str, int]) -> int:
    words.len()

def stride(n: int) -> int:
    for i in range(0, n, 2):
        pass
    1

def everywhere(mut items: list[Point]) -> int:
    for mut item in items:
        item.x = 1
    1
";
    let program = lowered(source).unwrap();
    let why: Vec<String> =
        program.not_lowered.iter().map(|skipped| format!("{}: {}", skipped.function, skipped.why)).collect();
    assert_eq!(
        why,
        [
            "make: it leaves a field of `Point` to its default",
            "checked: it gives back Checked",
            "pair: it gives back (int, int)",
            "squares: it uses a comprehension",
            "count: `words` is dict[str, int]",
            "stride: it has a `for` loop over a `range` stepping by something other than 1 or -1",
            "everywhere: it has a `for mut` loop",
        ]
    );
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
fn the_standard_librarys_fennec_functions_are_listed() {
    let source = "def starts(text: str) -> bool:
    text.startswith(\"a\")
";
    let program = lowered(source).unwrap();
    assert_eq!(program.external, ["str.startswith"]);
    assert_eq!(program.not_lowered, []);
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
    next.pos.x = body.pos.x + by
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
    _3 = _0.pos
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
    // How far lowering has got. This only goes up, until every function lowers.
    assert!(lowered_functions.len() >= 107, "only {} functions lowered", lowered_functions.len());
    for expected in
        ["numbers.fen: fib", "numbers.fen: collatz_steps", "numbers.fen: is_prime", "numbers.fen: lerp"]
    {
        assert!(lowered_functions.iter().any(|found| found == expected), "{expected} wasn't lowered");
    }
}
