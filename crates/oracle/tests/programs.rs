//! Small programs, each with what the oracle prints for it and how it ends.

use fennec_oracle::{Ended, Workdir, run_source_in, run_source_within};

/// Runs a program, with single quotes standing in for double ones and its indentation taken off,
/// in the folder the test runs in.
fn ran(text: &str) -> (String, Ended) {
    ran_in(text, Workdir::current())
}

/// Runs a program as `ran` does, in an empty folder at `/work`, which the disk is never looked at
/// for.
fn ran_in_work(text: &str) -> (String, Ended) {
    ran_in(text, Workdir::empty(std::path::Path::new("/work")))
}

fn prints_in_work(text: &str, expected: &str) {
    let (printed, ended) = ran_in_work(text);
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, expected);
}

fn ran_in(text: &str, workdir: Workdir) -> (String, Ended) {
    let text = text.replace('\'', "\"");
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = text.lines().map(|line| line.get(indent..).unwrap_or("")).collect();
    let source = format!("{}\n", lines.join("\n").trim_matches('\n'));
    let outcome = run_source_in(&source, "test.fen", workdir);
    (String::from_utf8_lossy(&outcome.stdout).into_owned(), outcome.ended)
}

fn prints(text: &str, expected: &str) {
    let (printed, ended) = ran(text);
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, expected);
}

#[test]
fn a_name_holds_its_own_value_and_ref_changes_yours() {
    prints(
        "
        struct Unit:
            name: str
            health = 100

        def heal_all(ref units: list[Unit]):
            for ref unit in units:
                unit.health += 10

        def main(stdout: Stdout):
            mut units = [Unit(name='Ana', health=50), Unit(name='Bo', health=70)]
            units[0].health -= 10
            heal_all(ref units)
            target = units[1]
            mut copied = units
            copied[0].health = 1
            stdout.print(units, target.health + 1, copied[0].health)
        ",
        "[Unit(name=\"Ana\", health=50), Unit(name=\"Bo\", health=80)] 81 1\n",
    );
}

/// What a method is called on is read before its arguments, left to right, as
/// `docs/decisions.md` says, and a closure before its own: an argument that changes it, like
/// `tally.show(tally.grow())`, leaves the call made on it as it was. What it prints is what the
/// compiled program prints.
#[test]
fn a_call_reads_what_it_is_made_on_before_its_arguments() {
    prints(
        "
        struct Box:
            items: list[int]
            count: int

            def show(self, given: int) -> str:
                return f'{self.items} {self.count} {given}'

            def grow(ref self) -> int:
                self.items.append(9)
                self.count += 1
                return self.count

            def again(ref self) -> str:
                return self.show(self.grow())

        struct Pair:
            left: Box
            right: int

            def shown(self, given: int) -> str:
                return f'{self.left.count} {self.right} {given}'

        def swap(ref called: (int) -> int) -> int:
            first = called(0)
            called = (x) => x * 10
            return first

        def main(stdout: Stdout):
            mut tally = Box(items=[1], count=0)
            stdout.print(tally.show(tally.grow()))
            stdout.print(tally.show(given=tally.grow()))
            stdout.print(tally.again(), tally.count)
            stdout.print(tally.show(0) + ' ' + tally.show(tally.grow()))
            stdout.print(tally.show(tally.grow()).upper(), tally.count)
            mut pair = Pair(left=Box(items=[], count=0), right=7)
            stdout.print(pair.shown(pair.left.grow()))
            stdout.print(pair.left.show(pair.left.grow()))
            mut boxes = [Box(items=[], count=0)]
            stdout.print(boxes[0].show(boxes[0].grow()))
            mut called: (int) -> int = (x) => x + 1
            stdout.print(called(swap(ref called)), called(1))
            return
        ",
        "[1] 0 1\n\
         [1, 9] 1 2\n\
         [1, 9, 9] 2 3 3\n\
         [1, 9, 9, 9] 3 0 [1, 9, 9, 9] 3 4\n\
         [1, 9, 9, 9, 9] 4 5 5\n\
         0 7 1\n\
         [9] 1 2\n\
         [] 0 1\n\
         2 10\n",
    );
}

/// `case .Button(ref clicks):` changes a field of what the `match` is over, a place reached
/// through a `ref` parameter and an index here: the name holds a copy, put back where the case
/// ends, however it ends, a failure included. A `ref` name matched again with `ref` puts its own
/// change back first. What it prints is what the compiled program printed.
#[test]
fn a_case_with_ref_changes_the_field_it_names() {
    prints(
        "
        enum Kind:
            Button(clicks: int)
            Label(text: str)
            Slider(at: int, most: int)

        struct Widget:
            name: str
            kind: Kind

        enum Named:
            Leaf(name: str)
            Pair(left: Named, right: Named, tags: list[str])

        error Refused:
            code: int

        def click(ref widgets: list[Widget], at: int):
            match widgets[at].kind:
                case .Button(ref clicks):
                    clicks += 1
                case .Slider(ref pos, most):
                    if pos < most:
                        pos += 10
                case .Label(ref text):
                    text += '?'

        def slide(ref kind: Kind) fails Refused:
            match kind:
                case .Slider(ref pos, ref most):
                    most += 1
                    pos += 5
                    if pos > 6:
                        fail Refused(code=pos)
                case _:
                    pass

        def main(stdout: Stdout):
            mut widgets = [
                Widget(name='go', kind=.Button(0)),
                Widget(name='vol', kind=.Slider(at=1, most=50)),
                Widget(name='hi', kind=.Label('hello')),
            ]
            for at in range(3):
                click(ref widgets, at)
            click(ref widgets, 0)
            stdout.print(widgets)
            mut tree = Named.Pair(left=.Leaf('a'), right=.Leaf('c'), tags=['t'])
            match tree:
                case .Pair(left=ref left, right=_, tags=ref tags):
                    tags.append('u')
                    match left:
                        case .Leaf(ref name):
                            name += '!'
                        case _:
                            pass
                case _:
                    pass
            stdout.print(tree)
            mut kind = Kind.Slider(at=1, most=9)
            slide(ref kind) except why:
                stdout.print(f'{why}')
            stdout.print(kind)
            slide(ref kind) except why:
                stdout.print(f'{why}')
            stdout.print(kind)
        ",
        "[Widget(name=\"go\", kind=Kind.Button(clicks=2)), Widget(name=\"vol\", kind=Kind.Slider(at=11, most=50)), \
         Widget(name=\"hi\", kind=Kind.Label(text=\"hello?\"))]\n\
         Named.Pair(left=Named.Leaf(name=\"a!\"), right=Named.Leaf(name=\"c\"), tags=[\"t\", \"u\"])\n\
         Kind.Slider(at=6, most=10)\nRefused(code=11)\nKind.Slider(at=11, most=11)\n",
    );
}

/// A `ref` case is put back into the place it matched however it's written: three `match`es deep,
/// a part given a whole new value, parts named out of their fields' order or left out, a `ref`
/// name a parameter the place reads has too, a guard reading a `ref` name, and a body that changes
/// an index of the place. The compiled program put several of these back wrong, or stopped
/// building, until the checker lane's fix; these are its programs, and what it prints now
/// (`llvm::a_ref_case_is_put_back_where_it_matched_however_it_s_written`).
#[test]
fn a_ref_case_is_put_back_where_it_matched_however_it_s_written() {
    prints(
        "
        enum Named:
            Leaf(name: str)
            Pair(left: Named, tags: list[str])

        def main(stdout: Stdout):
            mut tree = Named.Pair(left=.Pair(left=.Leaf(name='a'), tags=[]), tags=[])
            match tree:
                case .Pair(left=ref left, tags=_):
                    match left:
                        case .Pair(left=ref inner, tags=_):
                            match inner:
                                case .Leaf(ref name):
                                    name += '!'
                                case _:
                                    pass
                        case _:
                            pass
                case _:
                    pass
            stdout.print(tree)
            mut other = Named.Pair(left=.Leaf(name='b'), tags=['t'])
            match other:
                case .Pair(tags=ref tags, left=ref left):
                    tags.append('u')
                    left = .Leaf(name='replaced')
                case _:
                    pass
            stdout.print(other)
        ",
        "Named.Pair(left=Named.Pair(left=Named.Leaf(name=\"a!\"), tags=[]), tags=[])\n\
         Named.Pair(left=Named.Leaf(name=\"replaced\"), tags=[\"t\", \"u\"])\n",
    );
    prints(
        "
        enum Kind:
            Button(clicks: int)
            Slider(at: int, most: int)

        def click(ref kinds: list[Kind], at: int):
            match kinds[at]:
                case .Slider(ref at, most) if at < most:
                    at += 10
                case .Button(ref clicks):
                    clicks += 1
                case _:
                    pass

        def main(stdout: Stdout):
            mut kinds = [Kind.Button(clicks=0), Kind.Slider(at=1, most=50), Kind.Slider(at=60, most=50)]
            for at in range(3):
                click(ref kinds, at)
            stdout.print(kinds)
        ",
        "[Kind.Button(clicks=1), Kind.Slider(at=11, most=50), Kind.Slider(at=60, most=50)]\n",
    );
    prints(
        "
        enum Slot:
            Empty
            Held(count: int)

        def main(stdout: Stdout):
            mut slots = [Slot.Held(count=1), Slot.Held(count=10)]
            mut at = 0
            match slots[at]:
                case .Held(ref count):
                    at += 1
                    count += 5
                case _:
                    pass
            stdout.print(slots, at)
        ",
        "[Slot.Held(count=6), Slot.Held(count=10)] 1\n",
    );
}

/// A `ref` case is put back with its own names, past names inside its body that a `ref` name has
/// too, a loop's, a nested function's parameter or a `with`'s, and from an `except` block. This is
/// the checker lane's program, and what the compiled program prints with its fix.
#[test]
fn a_ref_case_is_put_back_past_names_inside_it() {
    prints(
        "
        enum Kind:
            Slider(value: int)
            Empty

        error Oops:
            code: int

        def opened() -> with int:
            value = 5
            lend value

        def risky(flag: bool) -> int fails Oops:
            if flag: fail Oops(code=1)
            return 0

        def main(stdout: Stdout):
            mut looped = Kind.Slider(value=1)
            mut seen = 0
            mode = 0
            match looped:
                case .Slider(ref value):
                    match mode:
                        case 0:
                            for value in [10, 20]:
                                seen += value
                        case _:
                            pass
                    value += 1
                case _:
                    pass
            mut nested = Kind.Slider(value=1)
            match nested:
                case .Slider(ref value):
                    def bumped(value: int) -> int:
                        return value + 100
                    value = bumped(5)
                case _:
                    pass
            mut lent = Kind.Slider(value=1)
            match lent:
                case .Slider(ref value):
                    with opened() as added:
                        value += added
                case _:
                    pass
            mut handled = Kind.Slider(value=1)
            caught = risky(True) except why:
                match handled:
                    case .Slider(ref value):
                        value += 7
                    case _:
                        pass
                why.code
            stdout.print(looped, seen, nested, lent, handled, caught)
        ",
        "Kind.Slider(value=2) 30 Kind.Slider(value=105) Kind.Slider(value=6) Kind.Slider(value=8) 1\n",
    );
}

/// A failure leaving a `ref` case from a `?` in a `while` condition or after `and` puts it back
/// with its own names, not a loop's or an inner case's. This is the checker lane's program, and
/// what the compiled program prints with its fix.
#[test]
fn a_ref_case_left_by_a_failure_is_put_back_with_its_own_names() {
    prints(
        "
        error Oops:
            code: int

        enum Kind:
            Slider(value: int)
            Empty

        enum Named:
            Leaf(name: str)
            Pair(left: Named, name: str)

        def small(count: int) -> bool fails Oops:
            if count > 1:
                fail Oops(code=count)
            return count < 1

        def bump(ref kind: Kind) fails Oops:
            match kind:
                case .Slider(ref value):
                    value = 100
                    for value in [5]:
                        while small(value)?:
                            pass
                case _:
                    pass

        def renamed(ref named: Named, deep: bool) fails Oops:
            match named:
                case .Pair(ref left, ref name):
                    name = 'outer'
                    match left:
                        case .Leaf(ref name):
                            name = 'inner'
                            if deep and small(7)?:
                                pass
                        case _:
                            pass
                case _:
                    pass

        def main(stdout: Stdout):
            mut kind = Kind.Slider(value=0)
            bump(ref kind) except why:
                stdout.print(why.code)
            mut named = Named.Pair(left=.Leaf(name='a'), name='b')
            renamed(ref named, deep=True) except why:
                stdout.print(why.code)
            stdout.print(kind, named)
        ",
        "5\n7\nKind.Slider(value=100) Named.Pair(left=Named.Leaf(name=\"inner\"), name=\"outer\")\n",
    );
}

/// A copy changed after it was made from another, or after it was sent to a task, changes alone,
/// at every level: a list, a list of lists, tuples in a list, a set, a dict of lists, a struct and
/// a struct of structs, lists and dicts. So do an argument, a `for ref` loop's list and what an
/// arrow captured, while a `ref` argument changes its caller's. The oracle shares a value's parts
/// between its copies until one is changed, and this holds it to the compiled program, which
/// printed the same at 1, 4 and 8 threads (`workers/copies_changed_after_sharing.fen`).
#[test]
fn a_copy_changed_after_sharing_changes_alone() {
    let (printed, ended) =
        ran(include_str!("../../fennec/tests/fixtures/workers/copies_changed_after_sharing.fen"));
    assert_eq!(ended, Ended::Exited(0), "{printed}");
    assert_eq!(printed, include_str!("../../fennec/tests/fixtures/workers/copies_changed_after_sharing.out"));
}

/// A value nested 200,000 deep, as `fennec-fuzz` writes them, is made, copied, compared, printed
/// and let go of: making one level a time copies nothing below it, and letting go of it takes
/// a little stack. One nested deeper than a walk through it may go on its thread's stack is
/// refused where it's printed. What it prints is what the compiled program printed.
#[test]
fn a_value_nested_200000_deep_is_made_and_let_go_of() {
    prints(
        "
        enum Node:
            Leaf(name: str)
            Branch(label: str, children: list[Node])

        def deep(depth: int, label: str) -> Node:
            mut node = Node.Leaf(name=label)
            for _ in range(depth):
                node = .Branch(label=label, children=[node])
            return node

        def depth_of(node: Node) -> int:
            mut count = 0
            mut at = node
            while True:
                match at:
                    case .Branch(children=children):
                        count += 1
                        at = children[0]
                    case .Leaf():
                        return count

        def main(stdout: Stdout):
            tall = deep(200000, 'hé🦊')
            mut kept = tall
            stdout.print(depth_of(kept), kept == tall)
            kept = deep(3, 'x')
            stdout.print(depth_of(tall), depth_of(kept), kept == tall)
            small = deep(20000, 'a')
            stdout.print(len(f'{small}'))
        ",
        "200000 True\n200000 3 False\n700019\n",
    );
    let (_, ended) = ran("
        enum Node:
            Leaf(name: str)
            Branch(label: str, children: list[Node])

        def main(stdout: Stdout):
            mut node = Node.Leaf(name='a')
            for _ in range(300000):
                node = .Branch(label='a', children=[node])
            stdout.print(node)
        ");
    assert_eq!(ended, Ended::Unsupported("ran too long: a value nested too deeply".to_owned()));
}

#[test]
fn except_handles_the_failure_of_its_call() {
    prints(
        "
        error Missing:
            name: str

        def find(names: list[str], name: str) -> int fails Missing:
            for at, each in enumerate(names):
                if each == name:
                    return at
            fail Missing(name)

        def main(stdout: Stdout):
            names = ['ash', 'kit']
            found = find(names, 'kit') except why:
                stdout.print('no', why)
                -1
            stdout.print(found)
            lost = find(names, 'vex') except why:
                case Missing(name):
                    stdout.print('missing', name)
                    -1
            stdout.print(lost)
        ",
        "1\nmissing vex\n-1\n",
    );
}

#[test]
fn a_failure_out_of_main_stops_the_program_with_exit_code_1() {
    let (printed, ended) = ran("
        error Broken:
            reason: str

        def main(stdout: Stdout):
            stdout.print('before')
            fail Broken('on purpose')
        ");
    assert_eq!(printed, "before\n");
    let Ended::Stopped(said) = ended else { panic!("{ended:?}") };
    assert!(
        said.starts_with("error: `main` failed: Broken(reason=\"on purpose\")\n --> test.fen:6:5"),
        "{said}"
    );
}

#[test]
fn an_int_that_overflows_stops_the_program() {
    let (printed, ended) = ran("
        def factorial(n: int) -> int:
            if n <= 1: return 1
            return n * factorial(n - 1)

        def main(stdout: Stdout):
            stdout.print(factorial(20))
            stdout.print(factorial(21))
        ");
    assert_eq!(printed, "2432902008176640000\n");
    assert!(matches!(&ended, Ended::Stopped(said) if said.contains("overflows an int")), "{ended:?}");
}

/// The oracle checks a program with the checker compiled code is checked with, so a `list[int]`
/// passed with `ref` where a `list[int?]` is changed is refused before it runs, in the checker's
/// words (#2596).
#[test]
fn a_ref_argument_that_holds_less_is_refused_before_it_runs() {
    let (printed, ended) = ran("
        def add_missing(ref items: list[int?]):
            items.append(None)
            return

        def main(stdout: Stdout):
            mut numbers = [1, 2]
            add_missing(ref numbers)
            total = numbers[2] + 1
            stdout.print(numbers, total)
            return
        ");
    assert_eq!(printed, "");
    assert!(
        matches!(&ended, Ended::Unsupported(said) if said.contains("so it could put in what this, a list[int], can't hold")),
        "{ended:?}"
    );
}

/// A generator held in a name fails through its loop as what it fails with, in a function that fails
/// with a union of errors, as compiled code does since #2086.
#[test]
fn a_named_generator_s_failure_passes_up_as_its_own_into_a_union() {
    prints(NAMED_GENERATOR_LOOP_UNION, "1\nShut(name=\"other\")\n");
    prints(NAMED_GENERATOR_COMPREHENSION_UNION, "Shut(name=\"other\")\n");
    prints(NAMED_GENERATOR_GIVEN_TWICE_UNION, "1\nShut(name=\"second\")\n");
    prints(NAMED_GENERATOR_ZIPPED_UNION, "Shut(name=\"other\")\n");
    prints(NAMED_GENERATOR_LISTED_UNION, "Shut(name=\"listed\")\n");
    prints(GENERATOR_OVER_A_FAILING_ONE_UNION, "10\nShut(name=\"inner\")\n");
    prints(GENERATOR_THROUGH_A_GENERIC_UNION, "1\nShut(name=\"generic\")\n");
    prints(GENERATORS_KEPT_IN_A_LIST_UNION, "1\nShut(name=\"held\")\n");
    prints(GENERATORS_JOINED_UNION, "n0, n1, n2\nShut(name=\"joined\")\n");
}

const NAMED_GENERATOR_LOOP_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut | Wrapped:
    kept = other("other")?
    for each in kept:
        stdout.print(each)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const NAMED_GENERATOR_COMPREHENSION_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut | Wrapped:
    kept = other("other")?
    items = [each for each in kept]
    stdout.print(items)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const NAMED_GENERATOR_GIVEN_TWICE_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut | Wrapped:
    mut kept = other("first")?
    kept = other("second")?
    for each in kept:
        stdout.print(each)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const NAMED_GENERATOR_ZIPPED_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def made(name: str) -> Iterator[int] fails Shut:
    yield 10
    yield 20

def run(stdout: Stdout) fails Shut | Wrapped:
    kept = other("other")?
    for step, item in zip(made("made")? from Wrapped(why="zip"), [each for each in kept]):
        stdout.print(step, item)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const NAMED_GENERATOR_LISTED_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut | Wrapped:
    kept = other("listed")?
    stdout.print(list(kept))

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const GENERATOR_OVER_A_FAILING_ONE_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def outer() -> Iterator[int] fails Shut | Wrapped:
    for each in other("inner")?:
        yield each * 10

def run(stdout: Stdout) fails Shut | Wrapped:
    for each in outer()?:
        stdout.print(each)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const GENERATOR_THROUGH_A_GENERIC_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def same[T](value: T) -> T:
    return value

def run(stdout: Stdout) fails Shut | Wrapped:
    kept = same(other("generic")?)
    for each in kept:
        stdout.print(each)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const GENERATORS_JOINED_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def shown(n: int) -> str fails Wrapped:
    if n > 2:
        fail Wrapped(why=f"n{n}")
    return f"n{n}"

def run(stdout: Stdout) fails Shut | Wrapped:
    stdout.print(", ".join(shown(n)? for n in [0, 1, 2]))
    joined = ", ".join(f"{each}" for each in other("joined")?)
    stdout.print(joined)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const GENERATORS_KEPT_IN_A_LIST_UNION: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

def other(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut | Wrapped:
    held = [other("held")?]
    for kept in held:
        for each in kept:
            stdout.print(each)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

#[test]
fn a_generator_and_its_reader_take_turns() {
    prints(
        "
        def countdown(out: Stdout, start: int) -> Iterator[int]:
            out.print('counting')
            mut n = start
            while n > 0:
                yield n
                n -= 1
            return out.print('liftoff')

        def naturals() -> Iterator[int]:
            mut n = 0
            while True:
                yield n
                n += 1

        def main(stdout: Stdout):
            for n in countdown(stdout, 2):
                stdout.print('got', n)
            stdout.print(any(n > 1 for n in naturals()), list(enumerate(n * 2 for n in range(2))))
        ",
        "counting\ngot 2\ngot 1\nliftoff\nTrue [(0, 0), (1, 2)]\n",
    );
}

/// A loop's generator wraps what it fails with in the loop's `? from Wrap(...)` as it passes up,
/// and one made right inside `enumerate` or `zip` is the loop's too, let go as the loop is: the
/// oracle prints what the compiled program prints since #2041.
#[test]
fn a_loop_wraps_its_generator_s_failure_and_owns_one_inside_enumerate_or_zip() {
    prints(GENERATOR_WRAPPED, "closing wrapped\nOops(code=91) [\"Shut(name=\\\"wrapped\\\")\"]\n");
    prints(GENERATOR_ENUMERATED, "closing enumerated\nOops(code=91) [\"Shut(name=\\\"enumerated\\\")\"]\n");
    prints(
        GENERATOR_WRAPPED_IN_WITH,
        "closing wrapped\nfailed outer\nOops(code=91) [\"Shut(name=\\\"wrapped\\\")\"]\n",
    );
    prints(
        GENERATOR_ENUMERATED_IN_WITH,
        "closing enumerated\nfailed outer\nOops(code=91) [\"Shut(name=\\\"enumerated\\\")\"]\n",
    );
    prints(GENERATOR_WRAPPED_BREAK, "closing broken\nbroken Wrapped(why=\"the break\")\n");
    prints(GENERATOR_ZIPPED, "closing zipped\nzipped Shut(name=\"zipped\") []\n");
    prints(
        GENERATORS_ZIPPED_BOTH,
        "closing second\nclosing first\nShut(name=\"second\") [\"Shut(name=\\\"first\\\")\"]\n",
    );
    prints(GENERATOR_ZIPPED_IN_ORDER, "first\nsecond\nfirst second\n");
    prints(GENERATOR_ZIPPED_IN_ORDER_IN_WITH, "first\nchecked second\nsecond\nfirst second\nclosed outer\n");
    prints(GENERATOR_ZIPPED_SECOND_FAILS, "a 1\nfailed Shut(name=\"mid\")\n");
    prints(GENERATOR_ZIPPED_FIRST_FAILS, "a 1\nfailed Shut(name=\"mid\")\n");
    prints(GENERATOR_ENUMERATED_FAILS, "0 1\nfailed Shut(name=\"mid\")\n");
    prints(
        GENERATOR_ZIPPED_FIRST_FAILS_SECOND_PARKED,
        "1 1\nclosing second\nShut(name=\"first\") [\"Shut(name=\\\"second\\\")\"]\n",
    );
    prints(
        GENERATOR_ENUMERATED_WRAPPED_IN_WITH,
        "closing enumerated\nfailed outer\nOops(code=91) [\"Shut(name=\\\"enumerated\\\")\"]\n",
    );
}

/// Two failing generators zipped, each with a `? from` of its own or one with none: the loop's
/// failure is the one of the side that failed, wrapped as that side says, the first side asked
/// first. The oracle refused these before, not knowing which side failed. What each prints is what
/// the compiled program printed.
#[test]
fn zip_wraps_a_failure_as_the_side_that_failed_says() {
    prints(
        GENERATORS_ZIPPED_WRAPPED_APART,
        "failing at 2\nboth LostLeft(step=2)\nfailing at 2\nboth LostRight(step=2)\nfailing at 3\nboth \
         LostLeft(step=3)\nboth ran 110\nfailing at 0\nboth LostLeft(step=0)\nfailing at 0\nboth \
         LostRight(step=0)\nfailing at 2\nleft LostLeft(step=2)\nfailing at 2\nleft Shut(code=2)\nfailing at \
         3\nleft LostLeft(step=3)\nleft ran 110\nfailing at 2\nright Shut(code=2)\nfailing at 2\nright \
         LostRight(step=2)\nfailing at 3\nright Shut(code=3)\nright ran 110\n",
    );
}

const GENERATORS_ZIPPED_WRAPPED_APART: &str = r#"error Shut:
    code: int

error LostLeft:
    step: int

error LostRight:
    step: int

def failing(limit: int, at: int, stdout: Stdout) -> Iterator[int] fails Shut:
    for each in range(limit):
        if each == at:
            stdout.print("failing at", each)
            fail Shut(code=each)
        yield each

def both_wrapped(left_at: int, right_at: int, stdout: Stdout) -> int fails LostLeft | LostRight:
    mut total = 0
    for one, other in zip(failing(5, at=left_at, stdout=stdout)? from LostLeft(step=left_at), failing(5, at=right_at, stdout=stdout)? from LostRight(step=right_at)):
        total += one * 10 + other
    return total

def left_wrapped(left_at: int, right_at: int, stdout: Stdout) -> int fails LostLeft | Shut:
    mut total = 0
    for one, other in zip(failing(5, at=left_at, stdout=stdout)? from LostLeft(step=left_at), failing(5, at=right_at, stdout=stdout)?):
        total += one * 10 + other
    return total

def right_wrapped(left_at: int, right_at: int, stdout: Stdout) -> int fails Shut | LostRight:
    mut total = 0
    for one, other in zip(failing(5, at=left_at, stdout=stdout)?, failing(5, at=right_at, stdout=stdout)? from LostRight(step=right_at)):
        total += one * 10 + other
    return total

def main(stdout: Stdout):
    for left_at, right_at in [(2, 4), (4, 2), (3, 3), (9, 9), (0, 9), (9, 0)]:
        got = both_wrapped(left_at, right_at=right_at, stdout=stdout) except why:
            stdout.print("both", why)
            continue
        stdout.print("both ran", got)
    for left_at, right_at in [(2, 4), (4, 2), (3, 3), (9, 9)]:
        got = left_wrapped(left_at, right_at=right_at, stdout=stdout) except why:
            stdout.print("left", why)
            continue
        stdout.print("left ran", got)
    for left_at, right_at in [(2, 4), (4, 2), (3, 3), (9, 9)]:
        got = right_wrapped(left_at, right_at=right_at, stdout=stdout) except why:
            stdout.print("right", why)
            continue
        stdout.print("right ran", got)
"#;

/// A field lent alone by `ref`, to a call or a `for ref` loop's turn, is a plain value to whoever
/// has it, and its struct's `where` rule runs when it comes back: at the end of the call, however
/// it ends but by a crash, and of each turn, however that ends. A value put right before then is
/// fine. A whole struct lent by `ref` is checked at each write. What each prints is what the
/// compiled program printed (#2270).
#[test]
fn a_field_lent_by_ref_is_checked_when_it_comes_back() {
    let broken = |field: &str| format!("this breaks the `where` rule on `Stats.{field}`");
    for (program, printed, field) in [
        (WHERE_LENT_TO_A_CALL, "Stats(level=3)\nlowered to -2\n", "level"),
        (WHERE_LENT_TO_TURNS, "dipped 1\ndipped 2\nStats(counts=[1, 2])\nraised 5\nraised 6\n", "counts"),
        (WHERE_LENT_TO_A_FIRST_TURN, "turn 1 21\n", "counts"),
        (WHERE_LENT_TO_A_BROKEN_TURN, "breaking 21\n", "counts"),
        (WHERE_LENT_TO_A_FAILING_CALL, "", "level"),
        (WHERE_LENT_TO_A_RETURNING_TURN, "", "counts"),
    ] {
        let (got, ended) = ran(program);
        assert_eq!(got, printed, "{program}");
        assert!(matches!(&ended, Ended::Stopped(said) if said.contains(&broken(field))), "{ended:?}");
    }
}

const WHERE_LENT_TO_A_CALL: &str = r#"struct Stats:
    level: int where level >= 1

def lowered(ref level: int, by: int, stdout: Stdout):
    level -= by
    stdout.print("lowered to", level)

def dipped(ref level: int, by: int):
    level -= by
    level += by

def main(stdout: Stdout):
    mut stats = Stats(level=3)
    dipped(ref stats.level, by=5)
    stdout.print(stats)
    lowered(ref stats.level, by=5, stdout=stdout)
    stdout.print(stats)
"#;

const WHERE_LENT_TO_TURNS: &str = r#"struct Stats:
    counts = [1, 2] where sum(counts) < 10

def main(stdout: Stdout):
    mut stats = Stats()
    for ref each in stats.counts:
        each += 20
        each -= 20
        stdout.print("dipped", each)
    stdout.print(stats)
    for ref each in stats.counts:
        each += 4
        stdout.print("raised", each)
    stdout.print(stats)
"#;

const WHERE_LENT_TO_A_FIRST_TURN: &str = r#"struct Stats:
    counts = [1, 2] where sum(counts) < 10

def main(stdout: Stdout):
    mut stats = Stats()
    mut turn = 0
    for ref each in stats.counts:
        if turn == 0:
            each += 20
        else:
            each -= 20
        turn += 1
        stdout.print("turn", turn, each)
    stdout.print(stats)
"#;

const WHERE_LENT_TO_A_BROKEN_TURN: &str = r#"struct Stats:
    counts = [1, 2] where sum(counts) < 10

def main(stdout: Stdout):
    mut stats = Stats()
    for ref each in stats.counts:
        each += 20
        stdout.print("breaking", each)
        break
    stdout.print("after", stats)
"#;

const WHERE_LENT_TO_A_FAILING_CALL: &str = r#"error Nope:
    code: int

struct Stats:
    level: int where level >= 1

def lowered(ref level: int, by: int) fails Nope:
    level -= by
    if level < 0:
        fail Nope(code=level)

def tried(ref stats: Stats, stdout: Stdout) fails Nope:
    lowered(ref stats.level, by=5)?

def main(stdout: Stdout):
    mut stats = Stats(level=3)
    tried(ref stats, stdout=stdout) except why:
        stdout.print("failed", why)
    stdout.print("after", stats)
"#;

const WHERE_LENT_TO_A_RETURNING_TURN: &str = r#"struct Stats:
    counts = [1, 2] where sum(counts) < 10

def first_big(ref stats: Stats, stdout: Stdout) -> int:
    for ref each in stats.counts:
        each += 20
        return each
    return 0

def main(stdout: Stdout):
    mut stats = Stats()
    stdout.print("got", first_big(ref stats, stdout=stdout))
    stdout.print("after", stats)
"#;

/// What a program saves of its own values loads back as compiled code loads it, through JSON,
/// `std.pack` and TOML: a union, as a field and in a list, tagged with its member's name, and each
/// way JSON can fail to hold one; a `Table` with an item taken out and the `Id`s into it, through
/// `std.pack` and TOML; and a `where` rule a value read through `std.pack` breaks, failing the load
/// with the error's line and column. The oracle refused each before. What each prints is what the
/// compiled program printed.
#[test]
fn saving_holds_unions_tables_ids_and_rules_through_every_format() {
    for (program, printed) in [
        (
            SAVED_UNION_JSON_LOADS,
            r#"Holder(shape=Square(side=2.0))
refused `shape` is "Triangle", which is none of `Circle | Square`
refused `shape` has "Square" after its member, but a union is written as an object with its member as the only key
refused `shape` should be a Circle | Square, but it's an object
refused `shape.Circle` should be a Circle, but it's an int
refused `shape` should be a Circle | Square, but it's a str
refused `Circle` has no field "side", but `shape.Circle` has one
refused `shape` should be a Circle | Square, but it's null
refused `shape` should be a Circle | Square, but it's a list
"#,
        ),
        (
            SAVED_UNION_ROUND_TRIPS,
            r#"{"shape": {"Square": {"side": 2.0}}, "shapes": [{"Circle": {"radius": 1.5}}, {"Square": {"side": 3.0}}]}
Holder(shape=Square(side=2.0), shapes=[Circle(radius=1.5), Square(side=3.0)])
Holder(shape=Square(side=2.0), shapes=[Circle(radius=1.5), Square(side=3.0)])
"#,
        ),
        (
            SAVED_UNION_TOML,
            r"Holder(shape=Circle(radius=1.5))
",
        ),
        (
            SAVED_TABLES_THROUGH_PACK_AND_TOML,
            r#"pack {"units": [{"name": "ash", "target": null}, {"name": "kit", "target": 2}, {"name": "vex", "target": 0}], "gauges": [{"level": 1, "name": "g"}, {"level": 2, "name": "b"}], "picked": 1}
best = [0]

[[items]]
name = "one"

[[items]]
name = "two"

{"items": [{"name": "one"}, {"name": "two"}], "best": [0]}
34
Holder(order=Order.Move(steps=2), orders=[Order.Wait, Order.Move(steps=5)])
"#,
        ),
        (
            SAVED_RULE_THROUGH_PACK,
            r"refused Gauge.level breaks its `where` rule, at byte 25
",
        ),
    ] {
        let (got, ended) = ran(program);
        assert_eq!(ended, Ended::Exited(0), "{program}");
        assert_eq!(got, printed, "{program}");
    }
}

const SAVED_UNION_JSON_LOADS: &str = r#"from std import json

struct Circle:
    radius: float

struct Square:
    side: float

type Shape = Circle | Square

struct Holder:
    shape: Shape

def loaded(text: str, stdout: Stdout):
    got = json.loads[Holder](text) except why:
        stdout.print("refused", why)
        return
    stdout.print(got)

def main(stdout: Stdout):
    loaded("{\"shape\": {\"Square\": {\"side\": 2}}}", stdout)
    loaded("{\"shape\": {\"Triangle\": {\"side\": 2}}}", stdout)
    loaded("{\"shape\": {\"Circle\": {\"radius\": 1.5}, \"Square\": {\"side\": 2}}}", stdout)
    loaded("{\"shape\": {}}", stdout)
    loaded("{\"shape\": {\"Circle\": 3}}", stdout)
    loaded("{\"shape\": \"Circle\"}", stdout)
    loaded("{\"shape\": {\"Circle\": {\"side\": 1}}}", stdout)
    loaded("{\"shape\": null}", stdout)
    loaded("{\"shape\": [1]}", stdout)
"#;

const SAVED_UNION_ROUND_TRIPS: &str = r#"from std import json, pack

struct Circle:
    radius: float

struct Square:
    side: float

type Shape = Circle | Square

struct Holder:
    shape: Shape
    shapes: list[Shape]

def main(stdout: Stdout):
    held = Holder(shape=Square(side=2.0), shapes=[Circle(radius=1.5), Square(side=3.0)])
    text = json.dumps(held)
    stdout.print(text)
    back = json.loads[Holder](text) except why:
        stdout.print("refused", why)
        return
    stdout.print(back)
    again = pack.loads[Holder](pack.dumps(held)) except why:
        stdout.print("refused", why)
        return
    stdout.print(again)
"#;

const SAVED_UNION_TOML: &str = r#"from std import toml

struct Circle:
    radius: float

struct Square:
    side: float

type Shape = Circle | Square

struct Holder:
    shape: Shape

def main(stdout: Stdout):
    held = Holder(shape=Circle(radius=1.5))
    saved = toml.dumps(held)
    back = toml.loads[Holder](saved) except why:
        stdout.print("refused", why)
        return
    stdout.print(back)
"#;

const SAVED_TABLES_THROUGH_PACK_AND_TOML: &str = r#"from std import json, pack, toml
from std.collections import Id, Table

struct Gauge:
    level: int where level >= 0
    name = "g"

struct Unit:
    name: str
    target: Id[Unit]? = None

struct World:
    units = Table[Unit]()
    gauges = list[Gauge]()
    picked: Id[Unit]? = None

struct Item:
    name: str

struct Flat:
    items = Table[Item]()
    best = list[Id[Item]]()

enum Order:
    Wait
    Move(steps: int)

struct Holder:
    order: Order
    orders = list[Order]()

def main(stdout: Stdout):
    mut world = World()
    ash = world.units.add(Unit(name="ash"))
    gone = world.units.add(Unit(name="gone"))
    vex = world.units.add(Unit(name="vex", target=ash))
    _ = world.units.remove(gone)
    kit = world.units.add(Unit(name="kit", target=vex))
    world.picked = kit
    world.gauges = [Gauge(level=1), Gauge(level=2, name="b")]
    for name, packed in [("pack", pack.dumps(world))]:
        back = pack.loads[World](packed) except why:
            stdout.print("refused", why)
            return
        stdout.print(name, json.dumps(back))
    mut flat = Flat()
    one = flat.items.add(Item(name="one"))
    _ = flat.items.add(Item(name="two"))
    flat.best = [one]
    text = toml.dumps(flat)
    stdout.print(text)
    from_toml = toml.loads[Flat](text) except why:
        stdout.print("refused", why)
        return
    stdout.print(json.dumps(from_toml))
    bad = Gauge(level=1)
    raw = pack.dumps(bad)
    stdout.print(len(raw))
    held = Holder(order=.Move(steps=2), orders=[.Wait, .Move(steps=5)])
    again = pack.loads[Holder](pack.dumps(held)) except why:
        stdout.print("refused", why)
        return
    stdout.print(again)
"#;

const SAVED_RULE_THROUGH_PACK: &str = r#"from std import pack

struct Gauge:
    level: int where level >= 0

struct Loose:
    level: int

def main(stdout: Stdout):
    raw = pack.dumps(Loose(level=-4))
    got = pack.loads[Gauge](raw) except why:
        stdout.print("refused", why)
        return
    stdout.print(got)
"#;

/// A generator that can fail, given a name and let go of partway, leaves at its `yield` where
/// compiled code frees it: as its function ends, or as its name is given another. Each `with` it's
/// in runs its cleanup there, and a cleanup that fails stops the program, with compiled code's
/// words, even where the function was failing. One made in an inner block, or two let go of at
/// once, are still refused: compiled code frees the first later than the block's end, and the two
/// in an order it doesn't promise. What each prints is what the compiled program printed (#2127).
#[test]
fn a_generator_let_go_of_partway_runs_its_cleanup_where_compiled_code_frees_it() {
    let stopped = "a generator's cleanup failed as it was let go by its last owner: Full(left=0)";
    for (body, expected, stops) in [
        (LET_GO_AT_THE_END, "got 1\nafter the loop\nstill in run\nclosing a\n", true),
        (
            LET_GO_FOR_ANOTHER,
            "got 1\nbefore reassigning\nclosing first\nafter reassigning\ngot again 1\nend of run\nclosing second\nafter run\n",
            false,
        ),
        (LET_GO_AFTER_MORE, "got 1\nafter the loop\nlater 0\nlater 1\nclosing d\nafter run\n", false),
        (LET_GO_AS_IT_FAILS, "got 1\nfailing now\nclosing f\n", true),
        (LET_GO_IN_A_LIST, "got 1\nend of run\nclosing held\n", true),
    ] {
        let (printed, ended) = ran(&format!("{LETTING_GO}{body}"));
        assert_eq!(printed, expected, "{body}");
        if stops {
            assert!(matches!(&ended, Ended::Stopped(said) if said.contains(stopped)), "{ended:?}");
        } else {
            assert_eq!(ended, Ended::Exited(0));
        }
    }
    for body in [LET_GO_IN_AN_IF, LET_GO_TWO_AT_ONCE] {
        let (_, ended) = ran(&format!("{LETTING_GO}{body}"));
        assert!(matches!(ended, Ended::Unsupported(_)), "{ended:?}");
    }
}

const LETTING_GO: &str = r#"error Full:
    left: int

struct Door:
    name: str

def source(name: str, failing: bool, stdout: Stdout) -> with Door fails Full:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    if failing:
        fail Full(left=0)

def numbers(name: str, failing: bool, stdout: Stdout) -> Iterator[int] fails Full:
    with source(name, failing, stdout)? as door:
        yield 1
        yield 2
        yield 3

"#;

const LET_GO_AT_THE_END: &str = r#"def run(stdout: Stdout) fails Full:
    made = numbers("a", failing=True, stdout=stdout)?
    for n in made:
        stdout.print("got", n)
        break
    stdout.print("after the loop")
    stdout.print("still in run")

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
    stdout.print("after run")
"#;

const LET_GO_FOR_ANOTHER: &str = r#"def run(stdout: Stdout) fails Full:
    mut made = numbers("first", failing=False, stdout=stdout)?
    for n in made:
        stdout.print("got", n)
        break
    stdout.print("before reassigning")
    made = numbers("second", failing=False, stdout=stdout)?
    stdout.print("after reassigning")
    for n in made:
        stdout.print("got again", n)
        break
    stdout.print("end of run")

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
    stdout.print("after run")
"#;

const LET_GO_AFTER_MORE: &str = r#"def run(stdout: Stdout) fails Full:
    made = numbers("d", failing=False, stdout=stdout)?
    for n in made:
        stdout.print("got", n)
        break
    stdout.print("after the loop")
    for i in range(2):
        stdout.print("later", i)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
    stdout.print("after run")
"#;

const LET_GO_AS_IT_FAILS: &str = r#"error Other:
    code: int

def run(stdout: Stdout) fails Full | Other:
    made = numbers("f", failing=True, stdout=stdout)?
    for n in made:
        stdout.print("got", n)
        break
    stdout.print("failing now")
    fail Other(code=7)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
    stdout.print("after run")
"#;

const LET_GO_IN_A_LIST: &str = r#"def run(stdout: Stdout) fails Full:
    held = [numbers("held", failing=True, stdout=stdout)?]
    for made in held:
        for n in made:
            stdout.print("got", n)
            break
    stdout.print("end of run")

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
    stdout.print("after run")
"#;

const LET_GO_IN_AN_IF: &str = r#"def run(stdout: Stdout, go: bool) fails Full:
    if go:
        made = numbers("c", failing=False, stdout=stdout)?
        for n in made:
            stdout.print("got", n)
            break
        stdout.print("end of if")
    stdout.print("after if")

def main(stdout: Stdout):
    run(stdout, go=True) except why:
        stdout.print("failed", why)
    stdout.print("after run")
"#;

const LET_GO_TWO_AT_ONCE: &str = r#"def run(stdout: Stdout) fails Full:
    first = numbers("one", failing=False, stdout=stdout)?
    second = numbers("two", failing=False, stdout=stdout)?
    for n in first:
        stdout.print("first", n)
        break
    for n in second:
        stdout.print("second", n)
        break
    stdout.print("end of run")

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
    stdout.print("after run")
"#;

const GENERATORS_ZIPPED_BOTH: &str = r#"error Shut:
    name: str

struct Door:
    name: str

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def zipped(stdout: Stdout) -> int fails Shut:
    for left, right in zip(held_stuck("first", stdout)?, held_stuck("second", stdout)?):
        break
    return 0

def main(stdout: Stdout):
    got = zipped(stdout) except why:
        stdout.print(why, why.notes())
        return
    stdout.print(got)
"#;

const GENERATOR_ZIPPED_IN_ORDER: &str = r#"def noisy(word: str, stdout: Stdout) -> list[str]:
    stdout.print(word)
    return [word]

def counted(words: list[str]) -> Iterator[str]:
    for word in words:
        yield word

def main(stdout: Stdout):
    for first, second in zip(noisy("first", stdout), counted(noisy("second", stdout))):
        stdout.print(first, second)
"#;

const GENERATOR_ZIPPED_IN_ORDER_IN_WITH: &str = r#"error Oops:
    code: int

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    lend Door(name=name) except:
        stdout.print("failed", name)
    stdout.print("closed", name)

def noisy(word: str, stdout: Stdout) -> list[str]:
    stdout.print(word)
    return [word]

def checked(word: str, stdout: Stdout) -> str fails Oops:
    stdout.print("checked", word)
    if word == "":
        fail Oops(code=1)
    return word

def run(stdout: Stdout) fails Oops:
    with opened("outer", stdout) as door:
        for first, second in zip(noisy("first", stdout), noisy(checked("second", stdout)?, stdout)):
            stdout.print(first, second)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why)
"#;

const GENERATOR_ZIPPED_SECOND_FAILS: &str = r#"error Shut:
    name: str

def breaking(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut:
    for word, step in zip(["a", "b", "c"], breaking("mid")?):
        stdout.print(word, step)
    stdout.print("after")

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
"#;

const GENERATOR_ZIPPED_FIRST_FAILS: &str = r#"error Shut:
    name: str

def breaking(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut:
    for step, word in zip(breaking("mid")?, ["a", "b", "c"]):
        stdout.print(word, step)
    stdout.print("after")

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
"#;

const GENERATOR_ENUMERATED_FAILS: &str = r#"error Shut:
    name: str

def breaking(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut:
    for word, step in enumerate(breaking("mid")?):
        stdout.print(word, step)
    stdout.print("after")

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print("failed", why)
"#;

const GENERATOR_ZIPPED_FIRST_FAILS_SECOND_PARKED: &str = r#"error Shut:
    name: str

struct Door:
    name: str

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def breaking(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def run(stdout: Stdout) fails Shut:
    for step, other in zip(breaking("first")?, held_stuck("second", stdout)?):
        stdout.print(step, other)

def main(stdout: Stdout):
    run(stdout) except why:
        stdout.print(why, why.notes())
"#;

const GENERATOR_ENUMERATED_WRAPPED_IN_WITH: &str = r#"error Oops:
    code: int

error Wrapped:
    why: str

error Shut:
    name: str

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door except:
        stdout.print("failed", name)
    stdout.print("closed", name)

def kept(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    if name == "":
        fail Shut(name=name)
    with opened(name, stdout) as door:
        yield 1
        yield 2

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def risky(code: int) -> int fails Oops:
    if code > 5:
        fail Oops(code=code)
    return code

def enumerated(stdout: Stdout) fails Oops | Wrapped:
    with opened("outer", stdout) as door:
        for at, step in enumerate(held_stuck("enumerated", stdout)? from Wrapped(why="the loop")):
            fail Oops(code=90 + step + at)

def main(stdout: Stdout):
    enumerated(stdout) except why:
        stdout.print(why, why.notes())
"#;

const GENERATOR_WRAPPED: &str = r#"error Oops:
    code: int

error Wrapped:
    why: str

error Shut:
    name: str

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door except:
        stdout.print("failed", name)
    stdout.print("closed", name)

def kept(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    if name == "":
        fail Shut(name=name)
    with opened(name, stdout) as door:
        yield 1
        yield 2

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def risky(code: int) -> int fails Oops:
    if code > 5:
        fail Oops(code=code)
    return code

def wrapped(stdout: Stdout) fails Oops | Wrapped:
    for step in held_stuck("wrapped", stdout)? from Wrapped(why="the loop"):
        fail Oops(code=90 + step)

def main(stdout: Stdout):
    wrapped(stdout) except why:
        stdout.print(why, why.notes())
"#;

const GENERATOR_ENUMERATED: &str = r#"error Oops:
    code: int

error Wrapped:
    why: str

error Shut:
    name: str

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door except:
        stdout.print("failed", name)
    stdout.print("closed", name)

def kept(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    if name == "":
        fail Shut(name=name)
    with opened(name, stdout) as door:
        yield 1
        yield 2

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def risky(code: int) -> int fails Oops:
    if code > 5:
        fail Oops(code=code)
    return code

def enumerated(stdout: Stdout) fails Oops | Shut:
    for at, step in enumerate(held_stuck("enumerated", stdout)?):
        fail Oops(code=90 + step + at)

def main(stdout: Stdout):
    enumerated(stdout) except why:
        stdout.print(why, why.notes())
"#;

const GENERATOR_WRAPPED_IN_WITH: &str = r#"error Oops:
    code: int

error Wrapped:
    why: str

error Shut:
    name: str

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door except:
        stdout.print("failed", name)
    stdout.print("closed", name)

def kept(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    if name == "":
        fail Shut(name=name)
    with opened(name, stdout) as door:
        yield 1
        yield 2

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def risky(code: int) -> int fails Oops:
    if code > 5:
        fail Oops(code=code)
    return code

def wrapped(stdout: Stdout) fails Oops | Wrapped:
    with opened("outer", stdout) as door:
        for step in held_stuck("wrapped", stdout)? from Wrapped(why="the loop"):
            fail Oops(code=90 + step)

def main(stdout: Stdout):
    wrapped(stdout) except why:
        stdout.print(why, why.notes())
"#;

const GENERATOR_ENUMERATED_IN_WITH: &str = r#"error Oops:
    code: int

error Wrapped:
    why: str

error Shut:
    name: str

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door except:
        stdout.print("failed", name)
    stdout.print("closed", name)

def kept(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    if name == "":
        fail Shut(name=name)
    with opened(name, stdout) as door:
        yield 1
        yield 2

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def risky(code: int) -> int fails Oops:
    if code > 5:
        fail Oops(code=code)
    return code

def enumerated(stdout: Stdout) fails Oops | Shut:
    with opened("outer", stdout) as door:
        for at, step in enumerate(held_stuck("enumerated", stdout)?):
            fail Oops(code=90 + step + at)

def main(stdout: Stdout):
    enumerated(stdout) except why:
        stdout.print(why, why.notes())
"#;

const GENERATOR_WRAPPED_BREAK: &str = r#"error Shut:
    name: str

error Wrapped:
    why: str

struct Door:
    name: str

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def broken(stdout: Stdout) -> int fails Wrapped:
    for step in held_stuck("broken", stdout)? from Wrapped(why="the break"):
        if step == 1:
            break
    return 1

def main(stdout: Stdout):
    got = broken(stdout) except why:
        stdout.print("broken", why)
        return
    stdout.print("broken gave", got)
"#;

const GENERATOR_ZIPPED: &str = r#"error Shut:
    name: str

struct Door:
    name: str

def stuck(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held_stuck(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with stuck(name, stdout)? as door:
        yield 1
        yield 2

def zipped(stdout: Stdout) -> int fails Shut:
    mut total = 0
    for step, word in zip(held_stuck("zipped", stdout)?, ["a", "b", "c"]):
        total += step + len(word)
        if step == 1:
            break
    return total

def main(stdout: Stdout):
    got = zipped(stdout) except why:
        stdout.print("zipped", why, why.notes())
        return
    stdout.print("zipped gave", got)
"#;

/// A source's cleanup runs once, after what's inside its block is left: a `for ref` item written
/// back, a generator the block loops over let go, and one failing partway through still running it.
/// A test holds its failure the same way since #2040; the oracle runs these as functions, and
/// prints what the compiled program prints.
#[test]
fn a_source_s_cleanup_runs_once_what_s_inside_its_block_is_left() {
    prints(
        CLEANUP_AFTER_INSIDE,
        "cleanup sees [11, 2]\nOops(code=11)\ncleanup of outer sees 1\nOops(code=1)\nstep 1\ncleanup of outer sees 0\nShut(name=\"mid\")\n",
    );
}

const CLEANUP_AFTER_INSIDE: &str = r#"error Oops:
    code: int

error Shut:
    name: str

struct Door:
    name: str

def tallied(ref xs: list[int], stdout: Stdout) -> with int:
    lend 1 except:
        stdout.print("cleanup sees", xs)

def ref_item(stdout: Stdout) fails Oops:
    mut xs = [1, 2]
    with tallied(ref xs, stdout) as got:
        for ref x in xs:
            x += 10
            fail Oops(code=x + got * 0)

def opened(name: str, seen: shared list[str], stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door except:
        lock seen as now:
            stdout.print("cleanup of", name, "sees", len(now))
    lock seen as ref now:
        now.append(name)

def held(name: str, seen: shared list[str], stdout: Stdout) -> Iterator[int] fails Shut:
    with opened(name, seen, stdout) as door:
        yield 1
        yield 2

def breaking(name: str) -> Iterator[int] fails Shut:
    yield 1
    fail Shut(name=name)

def inner_closes(stdout: Stdout) fails Oops | Shut:
    shared seen = list[str]()
    with opened("outer", seen, stdout) as door:
        for step in held("inner", seen, stdout)?:
            fail Oops(code=step)

def failing_mid(stdout: Stdout) fails Shut:
    shared seen = list[str]()
    with opened("outer", seen, stdout) as door:
        for step in breaking("mid")?:
            stdout.print("step", step)

def main(stdout: Stdout):
    ref_item(stdout) except why:
        stdout.print(why)
    inner_closes(stdout) except why:
        stdout.print(why)
    failing_mid(stdout) except why:
        stdout.print(why)
"#;

/// A loop over a generator made for it lets it go when it's left early, as compiled code does: the
/// generator leaves as if by `return`, so the `with` it's stopped in runs its cleanup there and
/// then. A cleanup's failure takes the place of a `break` or `return`, and is noted on a failure
/// leaving the loop. One let go outside any `with` runs nothing, and one a type's `iter` makes is
/// the loop's too. The output is what the compiled program printed.
#[test]
fn a_loop_lets_go_of_its_generator_as_it_is_left() {
    prints(
        LOOP_LETS_GO,
        "opening 2\nclosing 2\nbroken even failed Shut(code=2)\nopening 3\nclosing 3\nafter the loop 3\nbroken odd 3\nopening 4\nclosing 4\nreturned even failed Shut(code=4)\nopening 5\nclosing 5\nreturned odd 50\nopening 2\nclosing 2\nfinished even failed Shut(code=2)\nopening 2\nclosing 2\nfailed even Oops(code=2) [\"Shut(code=2)\"]\nopening 3\nclosing 3\nfailed odd Oops(code=3) []\nplain 1\ncounter 1\n",
    );
}

/// A consumer of a generator that fails partway, inside a statement, passes the failure up before
/// the rest of the statement runs, as compiled code now does (#2098). The output is what the
/// compiled program printed.
#[test]
fn a_consumer_of_a_failing_generator_fails_before_its_statement_goes_on() {
    prints(
        CONSUMER_FAILS_FIRST,
        "failed Bad(code=41)\nfailed Bad(code=42)\nfailed Bad(code=43)\nfailed Bad(code=44)\nfailed Bad(code=45)\nfailed Bad(code=46)\nfailed Bad(code=47)\nfailed Bad(code=48)\nopening\ncleaning up\nfailed Bad(code=49)\nfailed Bad(code=51)\nfailed Bad(code=52)\ncounted 0\nfailed Bad(code=53)\nlocked 0\nfailed Bad(code=54)\nopening\ncleaning up\nfailed Bad(code=50)\n",
    );
}

const CONSUMER_FAILS_FIRST: &str = "
        # A consumer of a generator that fails partway, inside a statement, passes the failure up before
        # the rest of the statement runs: nothing is printed with a partial value, nothing written after
        # it in the statement is called, nothing is yielded or written to a field, and `max` never sees an
        # empty list. Each generator yields once, or never, then fails.

        error Bad:
            code: int

        struct Tally:
            count: int

        def second(code: int) -> Iterator[int] fails Bad:
            yield 2
            fail Bad(code=code)

        def nothing_then(code: int) -> Iterator[int] fails Bad:
            for each in range(0):
                yield each
            fail Bad(code=code)

        def totals(code: int) -> Iterator[int] fails Bad:
            yield sum(second(code)?)
            yield 7

        def noisy(stdout: Stdout, said: str) -> int:
            stdout.print('noisy', said)
            return 1

        def pair(left: int, right: int) -> int:
            return left + right

        def opened(stdout: Stdout) -> with Tally:
            stdout.print('opening')
            lend Tally(count=1) except:
                stdout.print('cleaning up')
            stdout.print('closing')

        def summed(stdout: Stdout) fails Bad:
            stdout.print('the sum is', sum(second(41)?))

        def listed(stdout: Stdout) fails Bad:
            stdout.print('the list is', list(second(42)?))

        def doubled(stdout: Stdout) fails Bad:
            stdout.print('the doubles are', [each * 2 for each in second(43)?])

        def written_in(stdout: Stdout) fails Bad:
            stdout.print('the sum is', sum(each for each in second(44)?))

        def in_a_list(stdout: Stdout) fails Bad:
            shown = [sum(second(45)?), noisy(stdout, 'in a list')]
            stdout.print('listed', shown)

        def an_argument(stdout: Stdout) fails Bad:
            stdout.print('called', pair(left=max(second(46)?), right=noisy(stdout, 'an argument')))

        def formatted(stdout: Stdout) fails Bad:
            stdout.print(f'formatted {any(each > 5 for each in second(47)?)}')

        def in_order(stdout: Stdout) fails Bad:
            stdout.print('sorted', sorted(second(48)?), noisy(stdout, 'after sorted'))

        def in_a_with(stdout: Stdout) fails Bad:
            with opened(stdout) as tally:
                stdout.print('in the block', sum(second(49)?) + tally.count)

        def returned(stdout: Stdout) -> int fails Bad:
            with opened(stdout) as tally:
                return sum(second(50)?) + tally.count

        def yielded(stdout: Stdout) fails Bad:
            made = totals(51)?
            for total in made:
                stdout.print('total', total)

        def counted(ref tally: Tally) fails Bad:
            tally.count = sum(second(52)?)

        def locked(tally: shared Tally) fails Bad:
            lock tally as ref held:
                held.count = sum(second(53)?)

        def biggest(stdout: Stdout) fails Bad:
            stdout.print('the max is', max(nothing_then(54)?))

        def show(stdout: Stdout, run: (Stdout) -> nothing fails Bad):
            run(stdout) except why:
                stdout.print('failed', why)

        def main(stdout: Stdout):
            show(stdout, summed)
            show(stdout, listed)
            show(stdout, doubled)
            show(stdout, written_in)
            show(stdout, in_a_list)
            show(stdout, an_argument)
            show(stdout, formatted)
            show(stdout, in_order)
            show(stdout, in_a_with)
            show(stdout, yielded)
            mut tally = Tally(count=0)
            counted(ref tally) except why:
                stdout.print('failed', why)
            stdout.print('counted', tally.count)
            shared kept = Tally(count=0)
            locked(kept) except why:
                stdout.print('failed', why)
            lock kept as held:
                stdout.print('locked', held.count)
            show(stdout, biggest)
            got = returned(stdout) except why:
                stdout.print('failed', why)
                return
            stdout.print('got', got)
        ";

const LOOP_LETS_GO: &str = "
        error Shut:
            code: int

        error Oops:
            code: int

        struct Tally:
            count: int

        def shutting(code: int, stdout: Stdout) -> with Tally fails Shut:
            stdout.print('opening', code)
            lend Tally(count=code) except:
                stdout.print('undoing', code)
            stdout.print('closing', code)
            if code % 2 == 0:
                fail Shut(code=code)

        def counted(limit: int, stdout: Stdout) -> Iterator[int] fails Shut:
            with shutting(limit, stdout)? as tally:
                for at in range(limit):
                    yield at + tally.count

        def plain(limit: int) -> Iterator[int]:
            for at in range(limit):
                yield at

        def broken(limit: int, stop: int, stdout: Stdout) -> int fails Shut:
            mut total = 0
            for value in counted(limit, stdout)?:
                if value == stop:
                    break
                total += value
            stdout.print('after the loop', total)
            return total

        def returned(limit: int, stdout: Stdout) -> int fails Shut:
            for value in counted(limit, stdout)?:
                return value * 10
            return 0

        def failed(limit: int, stdout: Stdout) fails Shut | Oops:
            for value in counted(limit, stdout)?:
                fail Oops(code=value)

        def finished(limit: int, stdout: Stdout) -> int fails Shut:
            mut total = 0
            for value in counted(limit, stdout)?:
                total += value
            return total

        def plain_broken(stdout: Stdout) -> int:
            mut total = 0
            for value in plain(5):
                if value == 2:
                    break
                total += value
            return total

        struct Counter:
            limit: int

            def iter(self) -> Iterator[int]:
                for at in range(self.limit):
                    yield at

        def counter_broken(stdout: Stdout) -> int:
            mut seen = 0
            for value in Counter(limit=4):
                seen = value
                if value == 1:
                    break
            return seen

        def shown(what: str, stdout: Stdout, run: () -> int fails Shut):
            got = run() except why:
                stdout.print(what, 'failed', why)
                return
            stdout.print(what, got)

        def main(stdout: Stdout):
            shown('broken even', stdout, () => broken(2, stop=3, stdout=stdout)?)
            shown('broken odd', stdout, () => broken(3, stop=4, stdout=stdout)?)
            shown('returned even', stdout, () => returned(4, stdout)?)
            shown('returned odd', stdout, () => returned(5, stdout)?)
            shown('finished even', stdout, () => finished(2, stdout)?)
            failed(2, stdout) except why:
                stdout.print('failed even', why, why.notes())
            failed(3, stdout) except why:
                stdout.print('failed odd', why, why.notes())
            stdout.print('plain', plain_broken(stdout))
            stdout.print('counter', counter_broken(stdout))
        ";

/// A type a `with` source names as a type, and the `parallel` it calls, are its own, though its
/// caller names something the same: the oracle prints what the compiled program prints since #2039.
#[test]
fn a_with_source_s_type_names_are_its_own_though_the_caller_names_them() {
    prints(
        "
        enum Kind:
            Slider(total: int)
            Empty

        struct Door:
            name: str

            def made() -> Door:
                return Door(name='made')

        struct Gate:
            width: int

        struct Box[T]:
            item: T

        def door() -> Door:
            return Door(name='front')

        def made() -> with str:
            lend Door.made().name

        def narrowed(thing: Door | Gate) -> with str:
            mut said = 'gate'
            if thing is Door:
                said = thing.name
            lend said

        def boxed() -> with int:
            held = Box[int](item=3)
            lend held.item

        def cased() -> with int:
            kind = Kind.Slider(total=4)
            mut seen = 0
            match kind:
                case .Slider(total):
                    seen = total
                case _:
                    seen = 0
            lend seen

        def doubled(items: list[int]) -> with int:
            mut kept = items
            for ref item in parallel(kept):
                item *= 2
            lend sum(kept)

        def counted(Door: int, Box: int, Kind: int, parallel: int, stdout: Stdout):
            with made() as got:
                stdout.print(got, Door)
            with narrowed(door()) as got:
                stdout.print(got)
            with boxed() as got:
                stdout.print(got + Box)
            with cased() as got:
                stdout.print(got + Kind)
            with doubled([1, 2, 3]) as got:
                stdout.print(got + parallel)

        def main(stdout: Stdout):
            counted(Door=1, Box=2, Kind=3, parallel=4, stdout=stdout)
        ",
        "made 1\nfront\n5\n7\n16\n",
    );
}

#[test]
fn a_source_lends_to_the_block_and_finishes_after_it() {
    prints(
        "
        struct Ledger:
            lines = list[str]()

        def entry(ref ledger: Ledger, name: str) -> with str:
            ledger.lines.append(f'open {name}')
            lend name.upper() except:
                ledger.lines.append(f'undo {name}')
            ledger.lines.append(f'close {name}')

        def first(ref ledger: Ledger) -> int:
            with entry(ref ledger, 'bo') as label:
                return len(label)
            return 0

        def main(stdout: Stdout):
            mut ledger = Ledger()
            with entry(ref ledger, 'ana') as label:
                ledger.lines.append(label)
            stdout.print(first(ref ledger), ledger.lines)
        ",
        "2 [\"open ana\", \"ANA\", \"close ana\", \"open bo\", \"close bo\"]\n",
    );
}

/// A `with` source written in twice in one function, its union parameter given a different member
/// each time, both ways round, and a source written into another written in twice: the oracle
/// prints what the compiled program prints since #2056.
#[test]
fn a_source_used_twice_reads_each_union_member_it_is_given() {
    prints(
        "
        struct Door:
            name: str

        struct Gate:
            width: int

        def narrowed(thing: Door | Gate) -> with str:
            mut said = 'gate'
            if thing is Door:
                said = thing.name
            lend said

        def door_first() -> str:
            mut seen = ''
            with narrowed(Door(name='front')) as got:
                seen = got
            with narrowed(Gate(width=2)) as got:
                seen = seen + ' ' + got
            return seen

        def gate_first() -> str:
            mut seen = ''
            with narrowed(Gate(width=2)) as got:
                seen = got
            with narrowed(Door(name='front')) as got:
                seen = seen + ' ' + got
            return seen

        def twice(first: Door | Gate, second: Door | Gate) -> with str:
            mut seen = ''
            with narrowed(first) as got:
                seen = got
            with narrowed(second) as got:
                seen = seen + ' ' + got
            lend seen

        def nested() -> str:
            mut seen = ''
            with twice(Gate(width=1), Door(name='front')) as got:
                seen = got
            with twice(Door(name='back'), Gate(width=2)) as got:
                seen = seen + ' | ' + got
            return seen

        def main(stdout: Stdout):
            stdout.print(door_first())
            stdout.print(gate_first())
            stdout.print(nested())
        ",
        "front gate\ngate front\ngate front | back gate\n",
    );
}

/// An error carries notes: `add_note` adds one and `notes()` reads them, and printing it shows
/// none. A cleanup that fails for a failed block has its failure noted on the block's, as it
/// prints, when it's a `fail` or a line whose whole value is a `?`, and the rest of the cleanup is
/// skipped; a `?` deeper in a line takes the block's place instead, as `docs/language.md` says.
/// The oracle kept the block's failure every time, and gave `deep 3` where the compiled program
/// gives `deep 503`. What it prints is what the compiled program printed.
#[test]
fn an_error_carries_notes_and_a_failing_cleanup_notes_or_replaces_it() {
    prints(
        "
        error Refused:
            code: int

        struct Tally:
            count: int

        def guarded(code: int, stdout: Stdout) -> with Tally fails Refused:
            stdout.print('guard', code)
            lend Tally(count=code) except:
                stdout.print('unguard', code)
                if code % 3 == 0:
                    fail Refused(code=code + 1000)
                stdout.print('unguarded', code)
            stdout.print('released', code)

        def run(code: int, refuse: bool, stdout: Stdout) -> int fails Refused:
            with guarded(code, stdout)? as tally:
                if refuse:
                    fail Refused(code=tally.count)
                return tally.count

        def shown(code: int, refuse: bool, stdout: Stdout) -> str:
            value = run(code, refuse=refuse, stdout=stdout) except why:
                return f'refused {why.code} {why.notes()}'
            return str(value)

        def main(stdout: Stdout):
            stdout.print(shown(3, refuse=True, stdout=stdout))
            stdout.print(shown(4, refuse=True, stdout=stdout))
            stdout.print(shown(3, refuse=False, stdout=stdout))
            mut failure = Refused(code=9)
            failure.add_note('first')
            failure.add_note('second')
            stdout.print(failure.notes(), failure)
        ",
        "guard 3\nunguard 3\nrefused 3 [\"Refused(code=1003)\"]\nguard 4\nunguard 4\nunguarded 4\nrefused 4 []\n\
         guard 3\nreleased 3\n3\n[\"first\", \"second\"] Refused(code=9)\n",
    );
    prints(
        "
        error Refused:
            code: int

        struct Tally:
            count: int

        def capped(code: int) -> int fails Refused:
            if code > 2:
                fail Refused(code=code + 500)
            return code

        def plain(code: int, stdout: Stdout) -> with Tally fails Refused:
            lend Tally(count=code) except:
                kept = capped(code)?
                stdout.print('kept', kept)

        def deep(code: int, stdout: Stdout) -> with Tally fails Refused:
            lend Tally(count=code) except:
                stdout.print('deep', capped(code)?)

        def run_plain(code: int, stdout: Stdout) -> int fails Refused:
            with plain(code, stdout)? as tally:
                if tally.count > 0:
                    fail Refused(code=tally.count)
                return 0

        def run_deep(code: int, stdout: Stdout) -> int fails Refused:
            with deep(code, stdout)? as tally:
                if tally.count > 0:
                    fail Refused(code=tally.count)
                return 0

        def main(stdout: Stdout):
            first = run_plain(3, stdout) except why:
                stdout.print('plain', why.code)
                0
            second = run_deep(3, stdout) except why:
                stdout.print('deep', why.code)
                0
            stdout.print(first, second)
        ",
        "plain 3\ndeep 503\n0 0\n",
    );
}

/// Notes added to an error kept in a `shared` value, under its lock, are the error's own, as the
/// compiled program printed them (`workers/notes_kept_under_a_lock.fen`).
#[test]
fn notes_kept_under_a_lock_are_the_errors_own() {
    let (printed, ended) =
        ran(include_str!("../../fennec/tests/fixtures/workers/notes_kept_under_a_lock.fen"));
    assert_eq!(ended, Ended::Exited(0), "{printed}");
    assert_eq!(printed, include_str!("../../fennec/tests/fixtures/workers/notes_kept_under_a_lock.out"));
}

#[test]
fn a_loop_and_a_block_keep_their_names_to_themselves() {
    prints(
        "
        def main(stdout: Stdout):
            at = 9
            for at in range(3):
                stdout.print(at)
            stdout.print(at)
        ",
        "0\n1\n2\n9\n",
    );
}

/// A call that can fail gets its own line with `except`, and a chain from what it gave ends in
/// `else` at the first step that's missing.
#[test]
fn an_else_ends_a_chain_at_a_missing_step() {
    prints(
        "
        error Lost:
            why: str

        struct Part:
            name: str

        def find(n: int) -> Part fails Lost:
            if n > 1: fail Lost(why='no such part')
            return Part(name='gear')

        def main(stdout: Stdout):
            first = find(1) except Lost(): None
            second = find(2) except Lost(): None
            stdout.print(first.name else '?', second.name else '?')
            counts = {'a': 1}
            stdout.print(counts.get('b') else 0, counts.get('a') else 0)
        ",
        "gear ?\n0 1\n",
    );
}

#[test]
fn what_it_doesnt_model_is_refused_by_name() {
    let (printed, ended) = ran("
        from std import compress

        def main(stdout: Stdout):
            stdout.print('before')
            stdout.print(compress.gzip(bytes([1, 2])))
        ");
    assert_eq!(printed, "");
    assert_eq!(ended, Ended::Unsupported("std.compress.gzip".to_owned()));
    let (_, ended) = ran("
        def main(stdout: Stdout, process: Process):
            stdout.print('hi')
        ");
    assert_eq!(ended, Ended::Unsupported("the capability Process".to_owned()));
}

#[test]
fn a_run_that_goes_on_too_long_is_stopped() {
    let source = "def main(stdout: Stdout):\n    mut n = 0\n    while True:\n        n = (n + 1) % 7\n";
    let outcome = run_source_within(source, "test.fen", 100_000, Workdir::current());
    assert_eq!(outcome.ended, Ended::Unsupported("ran too long".to_owned()));
}

#[test]
fn a_buffer_is_held_as_the_list_it_prints_as() {
    prints(
        "
        def main(stdout: Stdout):
            mut buffer = Buffer[int](capacity=2)
            buffer.push(3)
            buffer.push(4)
            buffer.insert(0, 9)
            stdout.print(len(buffer), buffer.remove(1), buffer.pop(), buffer)
        ",
        "3 3 4 [9]\n",
    );
}

/// A buffer's room is known only as far as `docs/language.md` promises it: at least what it
/// holds, and right after `reserve(n)`, at least `n` more. Those comparisons answer, and what
/// they'd answer the same way whatever the runtime's growth rule. The number itself, or a
/// comparison past what's promised, is refused, and so is the reserved room once anything's
/// written. What it prints is what the compiled program printed.
#[test]
fn a_buffers_room_is_known_only_as_far_as_its_promised() {
    prints(
        "
        def main(stdout: Stdout):
            mut buffer = Buffer[int]()
            buffer.push(1)
            stdout.print(buffer.capacity() >= len(buffer), len(buffer) <= buffer.capacity())
            buffer.reserve(10)
            stdout.print(buffer.capacity() >= len(buffer) + 10, buffer.capacity() > 0)
        ",
        "True True\nTrue True\n",
    );
    let refused = Ended::Unsupported(
        "a Buffer's exact room, which `docs/language.md` promises only to be at least what it holds, \
         and n more right after `reserve(n)`"
            .to_owned(),
    );
    for after in [
        "stdout.print(buffer.capacity())",
        "stdout.print(buffer.capacity() >= 3)",
        "stdout.print(buffer.capacity() == 1)",
        "buffer.push(2)\n            stdout.print(buffer.capacity() >= 12)",
    ] {
        let (_, ended) = ran(&format!(
            "
        def main(stdout: Stdout):
            mut buffer = Buffer[int]()
            buffer.push(1)
            buffer.reserve(10)
            buffer.truncate(0)
            {after}
        "
        ));
        assert_eq!(ended, refused, "{after}");
    }
}

/// `std.pack` runs from its own source, and what it writes beside a value, the shape hash and the
/// layout, is worked out as `docs/pack.md` pins it: each of the doc's test vectors, from the same
/// program the LLVM test builds, is what the oracle writes too.
#[test]
fn pack_writes_the_headers_docs_pack_md_pins() {
    let doc = include_str!("../../../docs/pack.md");
    let vectors = doc.split("## Test vectors").nth(1).unwrap_or_default();
    let wanted: String = vectors
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells: Vec<String> = line
                .trim_matches('|')
                .split(" | ")
                .map(|cell| cell.trim().trim_matches('`').to_owned())
                .collect();
            match cells.as_slice() {
                [_, _, hash, layout] => format!("FPK2 {hash} {}\n", layout.replace("\\|", "|")),
                _ => unreachable!("a row of four: {line}"),
            }
        })
        .collect();
    assert!(wanted.lines().count() >= 15, "{wanted}");
    let (printed, ended) = ran(include_str!("../../fennec/tests/fixtures/pack_vectors.fen"));
    assert_eq!(ended, Ended::Exited(0), "{printed}");
    assert_eq!(printed, wanted);
}

/// An `f32` saves as itself in JSON, `std.pack` and TOML, as the compiled program prints it in
/// `llvm::an_f32_saves_as_itself_in_json_pack_and_toml`.
#[test]
fn an_f32_saves_as_itself() {
    let (printed, ended) = ran(include_str!("../../fennec/tests/fixtures/f32_saved.fen"));
    assert_eq!(ended, Ended::Exited(0), "{printed}");
    assert_eq!(printed, include_str!("../../fennec/tests/fixtures/f32_saved.out"));
}

/// A value saved with `pack.dumps` loads back with `pack.loads`, and data for another type is
/// refused by its shape hash. What it prints is what the compiled program printed.
#[test]
fn a_value_packed_loads_back() {
    prints(
        "
        from std import pack

        enum Mood:
            Calm
            Busy(level: int)

        struct Cell:
            name: str
            tags: list[str]
            mood: Mood

        struct Other:
            count: int

        def main(stdout: Stdout):
            data = pack.dumps(Cell(name='a', tags=['x', 'y'], mood=.Busy(level=3)))
            back = pack.loads[Cell](data) except ParseError():
                stdout.print('failed')
                return
            stdout.print(back.name, back.tags, back.mood)
            other = pack.loads[Other](data) except ParseError():
                stdout.print('refused')
                return
            stdout.print(other.count)
        ",
        "a [\"x\", \"y\"] Mood.Busy(level=3)\nrefused\n",
    );
}

#[test]
fn a_value_of_a_generic_type_prints_with_no_type_arguments() {
    prints(
        "
        struct Holder[T]:
            item: T

        struct Pair[A, B]:
            first: A
            second: B

        def main(stdout: Stdout):
            stdout.print(Holder(item=3), Holder(item='x'), Holder(item=2.5), [Holder(item=1)])
            stdout.print(f'{Holder(item=[1, 2])} {str(Pair(first=4, second=Holder(item=True)))}')
        ",
        "Holder(item=3) Holder(item=\"x\") Holder(item=2.5) [Holder(item=1)]\nHolder(item=[1, 2]) Pair(first=4, second=Holder(item=True))\n",
    );
}

#[test]
fn a_table_gives_each_item_an_id_that_finds_it_or_nothing() {
    let (printed, ended) = ran("
        from std.collections import Table

        struct Unit:
            health = 1

        def main(stdout: Stdout):
            mut names = Table[str]()
            first = names.add('a')
            _ = names.add('b')
            third = names.add('c')
            _ = names.remove(first)
            _ = names.remove(third)
            fourth = names.add('d')
            fifth = names.add('e')
            stdout.print(names, len(names), fourth, fifth, fourth == first, first in names, names.get(first))
            mut copied = names
            _ = copied.add('f')
            stdout.print(copied == names, names.items())
            mut units = Table[Unit]()
            unit_id = units.add(Unit())
            units[unit_id].health += 5
            for ref unit in units:
                unit.health *= 2
            stdout.print(units, units.update(unit_id, Unit(health=9)), units[unit_id].health)
            _ = units.remove(unit_id)
            stdout.print(units[unit_id])
        ");
    assert_eq!(
        printed,
        "Table([\"d\", \"b\", \"e\"]) 3 Id(0) Id(2) False False None\n\
         False [(Id(0), \"d\"), (Id(1), \"b\"), (Id(2), \"e\")]\n\
         Table([Unit(health=12)]) True 9\n"
    );
    let Ended::Stopped(said) = ended else { panic!("it ended {ended:?}") };
    assert!(
        said.starts_with(
            "error: this Id finds nothing in this Table: its item was taken out, or it's from another Table\n"
        ),
        "{said}"
    );
}

/// Writes a kit's files into a folder of its own, and runs it.
fn kit_ran(name: &str, files: &[(&str, &str)]) -> std::io::Result<(String, Ended)> {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("oracle-kits").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap_or(&dir))?;
        std::fs::write(&path, text.replace('\'', "\""))?;
    }
    let outcome = fennec_oracle::run_path(&dir, name);
    Ok((String::from_utf8_lossy(&outcome.stdout).into_owned(), outcome.ended))
}

const KIT_TOML: &str = "name = 'shapes'\nversion = '0.1.0'\nfennec = '0.0.1'\n\n[dependencies]\n";

#[test]
fn a_kit_is_its_modules_merged_as_the_compiler_merges_them() {
    let main = "\
from shapes import Shape, area as size_of
from world import physics
import world.weather as sky

def main(stdout: Stdout):
    shapes = [Shape.Circle(2), physics.square(3)]
    for shape in shapes:
        stdout.print(size_of(shape), physics.fall(shape))
    stdout.print(sky.today(), sky.Kind.Rain, physics.GRAVITY)
";
    let shapes = "\
enum Shape:
    Circle(radius: int)
    Square(side: int)

def area(shape: Shape) -> int:
    match shape:
        case .Circle(radius):
            return 3 * radius * radius
        case .Square(side):
            return side * side
";
    let physics = "\
from shapes import Shape, area

GRAVITY = 10

def square(side: int) -> Shape:
    return .Square(side)

def fall(shape: Shape) -> int:
    return area(shape) * GRAVITY
";
    // `Shape` has a `Square` too, but this file can't see it, so `.Square` is a `Kind`.
    let weather = "\
enum Kind:
    Sun
    Rain
    Square

def today() -> Kind:
    kind = .Square
    return kind
";
    let files = [
        ("kit.toml", KIT_TOML),
        ("src/main.fen", main),
        ("src/shapes.fen", shapes),
        ("src/world/physics.fen", physics),
        ("src/world/weather.fen", weather),
    ];
    let (printed, ended) = kit_ran("merged", &files).unwrap();
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, "12 120\n9 90\nKind.Square Kind.Rain 10\n");
    let depends =
        KIT_TOML.replace("[dependencies]\n", "[dependencies]\nphysics = { folder = '../physics' }\n");
    let (_, ended) = kit_ran("depends", &[("kit.toml", &depends), ("src/main.fen", main)]).unwrap();
    assert_eq!(ended, Ended::Unsupported("a kit with dependencies".to_owned()));
}

/// The files of a kit share one set of top-level names, as the compiler's loader holds them to:
/// two files defining `area`, each imported by a file of its own, is refused, where the oracle used
/// to run whichever `area` came last.
#[test]
fn a_name_two_files_of_a_kit_define_is_refused() {
    let main = "from squares import area\nimport report\n\ndef main(stdout: Stdout):\n    stdout.print(area(3), report.circle(3))\n";
    let files = [
        ("kit.toml", KIT_TOML),
        ("src/main.fen", main),
        ("src/squares.fen", "def area(side: int) -> int:\n    return side * side\n"),
        ("src/circles.fen", "def area(radius: int) -> int:\n    return 3 * radius * radius\n"),
        (
            "src/report.fen",
            "from circles import area\n\ndef circle(radius: int) -> int:\n    return area(radius)\n",
        ),
    ];
    let (_, ended) = kit_ran("two_areas", &files).unwrap();
    let Ended::Unsupported(why) = ended else { panic!("it ended {ended:?}") };
    assert!(why.contains("`area` is defined in `squares` and in `circles`"), "{why}");
}

/// What each file imports from the standard library is its own, as the checker has it: `pick` is
/// `floor` in the file that imports it so and `ceil` in the file that imports that, `dumps` is
/// JSON's in one and TOML's in the other, and the program's `id_at` is its own beside the
/// `id_at` `std.collections` imports for its `Deque`. The oracle refused each, where one table of
/// imports for every file couldn't hold both. What it prints is what the compiled program printed.
#[test]
fn what_each_file_imports_is_its_own() {
    let main = "\
from std.collections import Deque
from std.json import dumps
from std.math import floor as pick, sqrt as id_at
from shapes import Point
import other

def main(stdout: Stdout):
    stdout.print(pick(2.7), other.lifted(2.7), id_at(16.0))
    stdout.print(dumps(Point(x=1, y=2)))
    stdout.print(other.saved(Point(x=3, y=4)))
    mut queue = Deque.of([1, 2])
    queue.appendleft(0)
    stdout.print(len(queue), queue[0])
";
    let other = "\
from std.math import ceil as pick
from std.toml import dumps
from shapes import Point

def lifted(number: float) -> int:
    return pick(number)

def saved(point: Point) -> str:
    return dumps(point)
";
    let files = [
        ("kit.toml", KIT_TOML),
        ("src/main.fen", main),
        ("src/other.fen", other),
        ("src/shapes.fen", "struct Point:\n    x: int\n    y: int\n"),
    ];
    let (printed, ended) = kit_ran("own_imports", &files).unwrap();
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, "2 3 4.0\n{'x': 1, 'y': 2}\nx = 3\ny = 4\n\n3 0\n".replace('\'', "\""));
}

/// A program's own function with a builtin's name is its own, and std's code still calls the
/// builtin, by the checker's rule: std's `Tree.descendants` reverses with `reversed` and
/// `url.parse` fills in a `Url` with `copy`. The oracle ran the program's `reversed` from std's
/// code, and printed `right, left, leaf`. What it prints is what the compiled program printed.
#[test]
fn a_program_defining_builtin_names_changes_nothing_std_does() {
    prints(
        "
        from std import collections, url

        def copy(number: int) -> int:
            return number + 7

        def reversed(items: list[str]) -> list[str]:
            return items

        def main(stdout: Stdout):
            mut tree = collections.Tree[str]()
            root = tree.add('root')
            left = tree.add('left', parent=root)
            _ = tree.add('leaf', parent=left)
            _ = tree.add('right', parent=root)
            stdout.print(', '.join([tree[node] for node in tree.descendants(root)]))
            found = url.parse('http://example.com/a?b=1#top') except why:
                stdout.print(f'{why}')
                return
            stdout.print(found.scheme, found.fragment, found.path, copy(1), reversed(['a', 'b']))
        ",
        "left, leaf, right\nhttp top /a 8 [\"a\", \"b\"]\n",
    );
}

#[test]
fn json_writes_a_value_as_its_type_saves_it_and_reads_it_back() {
    prints(
        "
        from std import json

        enum Tile:
            Floor
            Door(locked: bool)

        struct Room:
            name: str
            tiles: list[Tile]
            doors: dict[str, int]
            corners: dict[(int, int), str]
            pet: str? = None
            @serialize(name='Size')
            size = (3, 4)
            @skip
            seen = 0

        def main(stdout: Stdout):
            room = Room(
                name='hall',
                tiles=[Tile.Floor, Tile.Door(locked=True)],
                doors={'north': 1},
                corners={(0, 0): 'start'},
            )
            text = json.dumps(room)
            stdout.print(text)
            stdout.print(json.dumps(room.doors, indent=2))
            back = json.loads[Room](text)?
            stdout.print(back == room, back.pet is None, back.seen)
            stdout.print(json.loads[list[int?]]('[1, null, 3]')?)
        ",
        "{\"name\": \"hall\", \"tiles\": [\"Floor\", {\"Door\": {\"locked\": true}}], \"doors\": {\"north\": 1}, \
         \"corners\": [[[0, 0], \"start\"]], \"pet\": null, \"Size\": [3, 4]}\n\
         {\n  \"north\": 1\n}\n\
         True True 0\n\
         [1, None, 3]\n",
    );
}

#[test]
fn json_writes_floats_to_read_back_the_same_and_escapes_only_what_it_must() {
    prints(
        "
        from std import json

        def main(stdout: Stdout):
            floats = [1e20, 1e-7, -0.0, 0.1, 100.0]
            stdout.print(json.dumps(floats))
            stdout.print(json.loads[list[float]](json.dumps(floats))? == floats)
            stdout.print(json.dumps('tab\\there \\'quoted\\' \\\\ \\u0001 \\u0085 é'))
            stdout.print(json.loads[str]('\\'\\\\u00e9t\\\\u00e9 \\\\ud83d\\\\ude00\\'')?)
        ",
        "[1e20, 1e-7, -0.0, 0.1, 100.0]\nTrue\n\"tab\\there \\\"quoted\\\" \\\\ \\u0001 \\u0085 é\"\nété 😀\n",
    );
}

/// `round` gives the float the runtime gives at every place. Its powers of ten are the exact ones
/// written out and, past those, the float nearest each: multiplying tens together gives another
/// float from ten to the 33 on, and rounding to those places then came out one bit off.
#[test]
fn rounding_scales_by_the_power_of_ten_nearest_it() {
    prints(
        "
        def main(stdout: Stdout):
            stdout.print([round(0.12345678901234568, places) for places in [1, 5, 9, 15, 16, 17, 22]])
            stdout.print([round(1.2345678901234567e40, places) for places in [-22, -23, -33, -34, -37, -39, -40]])
        ",
        "[0.1, 0.12346, 0.123456789, 0.123456789012346, 0.1234567890123457, 0.12345678901234568, \
         0.12345678901234568]\n[1.2345678901234566e+40, 1.2345678901234566e+40, 1.2345679e+40, 1.234568e+40, \
         1.235e+40, 1.2e+40, 1e+40]\n",
    );
}

/// `std.math` runs from its own Fennec source: its vectors, `dot`, and `lerp` taken under
/// another name with named arguments. `sin` and a float's `**` are `libm`'s, as the docs say,
/// an int given to `sqrt` is a float, and `reversed` of a tuple is a list. What it prints is what
/// the compiled program printed.
#[test]
fn std_math_runs_from_its_own_source() {
    prints(
        "
        from std import math
        from std.math import Vec3, lerp as blend, sin, sqrt

        def main(stdout: Stdout):
            a = Vec3(x=1.0, y=-2.0, z=3.0)
            b = Vec3.zero() + Vec3(x=0.5, y=0.5, z=0.5)
            stdout.print(a + b, abs(a - b), a.dot(b), a.cross(b))
            stdout.print(a.normalize().length(), math.dot([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]))
            stdout.print(blend(start=0.0, end=10.0, amount=0.25), sqrt(16), sin(1.0), 2.0 ** 0.3)
            stdout.print(reversed((1, 2, 3)), len([a, b]), abs(-1.5))
            return
        ",
        "Vec3(x=1.5, y=-1.5, z=3.5) Vec3(x=0.5, y=2.5, z=2.5) 1.0 Vec3(x=-2.5, y=1.0, z=1.5)\n\
         0.99999994 32.0\n\
         2.5 4.0 0.8414709848078965 1.2311444133449163\n\
         [3, 2, 1] 2 1.5\n",
    );
    // A program's own type with the name of one of the module's is its own, renamed as the checker
    // renames it, never mixed up with the module's.
    prints(
        "
        from std.math import sqrt

        struct Vec3:
            x: float

        def main(stdout: Stdout):
            stdout.print(Vec3(x=sqrt(4.0)))
        ",
        "Vec3(x=2.0)\n",
    );
}

/// `std.collections` runs from its own Fennec source too: a `Deque`, a `Heap` and a
/// `SortedMap`, with the `len(x)` its code has written as `x.len()` on a list and a dict. What
/// it prints is what the compiled program printed.
#[test]
fn std_collections_runs_from_its_own_source() {
    prints(
        "
        from std.collections import Deque, Heap, SortedMap

        def main(stdout: Stdout):
            mut queue = Deque.of([1, 2, 3])
            queue.append(4)
            first = queue.popleft() else 0
            stdout.print(queue, first, queue == Deque.of([2, 3, 4]))
            mut heap = Heap.of([5, 1, 4])
            heap.push(2)
            stdout.print(heap.pop() else 0, heap.pop() else 0, len(heap))
            mut counts = SortedMap[str, int]()
            for word in ['fox', 'den', 'kit', 'den']:
                counts[word] = (counts.get(word) else 0) + 1
            stdout.print(counts, counts.keys())
            return
        ",
        "Deque([2, 3, 4]) 1 True\n1 2 2\nSortedMap({\"den\": 2, \"fox\": 1, \"kit\": 1}) [\"den\", \"fox\", \"kit\"]\n",
    );
}

/// What's indexed is read before its index, left to right, as `docs/decisions.md` says: an index
/// that grows a list, or changes a dict's value, reads the list or dict from before. An index too
/// big for the list as it was stops the program, though the list has grown to hold it. What it
/// prints is what the compiled program printed, the same on three runs.
#[test]
fn what_is_indexed_is_read_before_its_index() {
    let (printed, ended) = ran(INDEXED_BEFORE_ITS_INDEX);
    assert_eq!(printed, "2 [1, 2, 3, 5]\n1 {\"a\": 101}\n3\n");
    let Ended::Stopped(said) = ended else { panic!("{ended:?}") };
    assert!(said.contains("index 1 is out of range for length 1"), "{said}");
}

/// A built-in method that only reads what it's called on reads it before its arguments, left to
/// right, as a method the program writes does (`a_call_reads_what_it_is_made_on_before_its_arguments`):
/// a dict's `get`, a list's `count` and a string's `split`, each given an argument that changes
/// what it's called on, see it as it was. What it prints is what the compiled program printed.
#[test]
fn a_built_in_method_that_only_reads_reads_what_it_is_called_on_first() {
    prints(
        "
        struct Tally:
            items: list[int]
            names: dict[str, int]
            text: str

            def key(ref self) -> str:
                self.names['a'] = self.names['a'] + 100
                return 'a'

            def grow(ref self) -> int:
                self.items.append(1)
                return 1

            def renamed(ref self) -> str:
                self.text = 'b,b'
                return ','

        def main(stdout: Stdout):
            mut tally = Tally(items=[1, 1], names={'a': 1}, text='a,a,a')
            stdout.print(tally.names.get(tally.key()), tally.names)
            stdout.print(tally.items.count(tally.grow()), tally.items)
            stdout.print(tally.text.split(tally.renamed()), tally.text)
            return
        ",
        "1 {\"a\": 101}\n2 [1, 1, 1]\n[\"a\", \"a\", \"a\"] b,b\n",
    );
}

const INDEXED_BEFORE_ITS_INDEX: &str = "
        struct Tally:
            items: list[int]
            count: int
            names: dict[str, int]

            def grow(ref self) -> int:
                self.items.append(self.count + 5)
                self.count += 1
                return self.count

            def key(ref self) -> str:
                self.names['a'] = self.names['a'] + 100
                return 'a'

        def main(stdout: Stdout):
            mut tally = Tally(items=[1, 2, 3], count=0, names={'a': 1})
            stdout.print(tally.items[tally.grow()], tally.items)
            stdout.print(tally.names[tally.key()], tally.names)
            stdout.print(tally.items[tally.count + 1])
            mut short = Tally(items=[1], count=0, names={'a': 1})
            stdout.print(short.items[short.grow()])
            return
        ";

/// Places through a type's own `def []`, as `docs/decisions.md` sets them out. Through the plain
/// form beside a `def []=`, a change reads the part, changes the copy and writes it back once,
/// nested too, and once for a method that changes it twice. Through the `ref` form a position is
/// a place, which `for ref`, `parallel`, a `ref` argument and `=` reach, its empty positions
/// skipped. What it prints is what the compiled program printed.
#[test]
fn a_types_own_index_reads_writes_back_and_gives_positions() {
    prints(
        "
        struct Part:
            name: str
            x = 0

            def moved(ref self, by: int):
                self.x += by
                self.x += by

        struct Shelf:
            parts: list[Part]
            writes = 0

            def [](self, at: int) -> Part:
                return self.parts[at]

            def []=(ref self, at: int, part: Part):
                self.parts[at] = part
                self.writes += 1

        struct Room:
            shelf: Shelf

            def [](self, name: str) -> Shelf:
                return self.shelf

            def []=(ref self, name: str, shelf: Shelf):
                self.shelf = shelf

        struct Slots:
            items = list[int]()
            alive = list[bool]()

            def len(self) -> int:
                return len(self.items)

            def is_live(self, at: int) -> bool:
                return self.alive[at]

            def [](ref self, at: int) -> int:
                return self.items[at]

        def bumped(ref value: int):
            value += 100

        def main(stdout: Stdout):
            mut shelf = Shelf(parts=[Part(name='bolt'), Part(name='nut')])
            shelf[0].x += 3
            shelf[1].moved(2)
            stdout.print(shelf[0].name, shelf.parts, shelf.writes)
            mut room = Room(shelf=shelf)
            room['back'][1].x = 9
            stdout.print(room.shelf.parts, room.shelf.writes, shelf.writes)
            mut slots = Slots(items=[1, 2, 3], alive=[True, False, True])
            for ref slot in slots:
                slot *= 10
            for ref slot in parallel(slots):
                slot += 1
            bumped(ref slots[1])
            slots[0] = 7
            stdout.print(slots.items, len(slots))
            return
        ",
        "bolt [Part(name=\"bolt\", x=3), Part(name=\"nut\", x=4)] 2\n\
         [Part(name=\"bolt\", x=3), Part(name=\"nut\", x=9)] 3 2\n\
         [7, 102, 31] 3\n",
    );
}

/// Through a type's own `def []`, every built-in method that changes its value is written back
/// through `def []=`, as compiled code does: a list's `sort`, with a key too, and `remove_where`,
/// and a table's `add`, `modify` and `remove_where`. One that only reads, a list's `count`,
/// `index` and `get` or a table's `values` and `get`, writes nothing back. What it prints is what
/// the compiled program printed, built for dev and with `--release`.
#[test]
fn every_change_through_a_types_own_index_is_written_back() {
    prints(
        "
        from std.collections import Table

        struct Point:
            x: int

        def bump(ref point: Point):
            point.x += 5

        struct Shelf:
            rows: list[list[int]]
            writes = 0

            def [](self, at: int) -> list[int]:
                return self.rows[at]

            def []=(ref self, at: int, row: list[int]):
                self.rows[at] = row
                self.writes += 1

        struct Desk:
            tables: list[Table[Point]]
            writes = 0

            def [](self, at: int) -> Table[Point]:
                return self.tables[at]

            def []=(ref self, at: int, table: Table[Point]):
                self.tables[at] = table
                self.writes += 1

        def main(stdout: Stdout):
            mut shelf = Shelf(rows=[[3, 1, 3], [5, 4, 6]])
            stdout.print(shelf[0].count(3), shelf[0].index(1), shelf[0].get(9), shelf.writes)
            shelf[0].sort()
            stdout.print(shelf.rows, shelf.writes)
            shelf[1].sort(key=item => 0 - item)
            stdout.print(shelf.rows, shelf.writes)
            shelf[1].remove_where(item => item > 5)
            stdout.print(shelf.rows, shelf.writes)
            mut desk = Desk(tables=[Table[Point]()])
            made = desk[0].add(Point(x=1))
            other = desk[0].add(Point(x=2))
            stdout.print(len(desk[0].values()), desk.writes)
            _ = desk[0].modify(made, bump)
            stdout.print(desk[0].get(made), desk.writes)
            desk[0].remove_where(point => point.x > 3)
            stdout.print(desk[0].get(made), desk[0].get(other), desk.writes)
            return
        ",
        "2 1 None 0\n\
         [[1, 3, 3], [5, 4, 6]] 1\n\
         [[1, 3, 3], [6, 5, 4]] 2\n\
         [[1, 3, 3], [5, 4]] 3\n\
         2 2\n\
         Point(x=6) 3\n\
         None Point(x=2) 4\n",
    );
}

/// A built-in method the oracle doesn't know changes its value or only reads it, called through
/// a type's own `def []`, is refused: writing it back or not could each be wrong.
#[test]
fn a_method_through_a_types_own_index_not_known_to_change_or_read_is_refused() {
    let (_, ended) = ran("
        struct Blobs:
            items: list[bytes]

            def [](self, at: int) -> bytes:
                return self.items[at]

            def []=(ref self, at: int, item: bytes):
                self.items[at] = item

        def main(stdout: Stdout):
            mut blobs = Blobs(items=['hi'.encode()])
            stdout.print(blobs[0].hex())
            return
        ");
    assert_eq!(
        ended,
        Ended::Unsupported(
            "the method `hex` through a type's own `def []`, not known to change its value or only read it"
                .to_owned()
        )
    );
}

/// A change through a type's own `def []=` that fails partway keeps what it changed before the
/// failure: the part is written back, since only `lock ... as ref` rolls back. What it prints is
/// what the compiled program printed, built for dev and with `--release`.
#[test]
fn a_change_through_its_own_index_that_fails_partway_is_written_back() {
    prints(
        "
        error Nope:
            code: int

        struct Part:
            name: str
            x: int
            marks: list[int]

            def shift(ref self, by: int) fails Nope:
                self.x += by
                self.marks.append(by)
                if by > 5:
                    fail Nope(code=by)
                self.x += 100

        struct Shelf:
            parts: dict[str, Part]

            def [](self, key: str) -> Part:
                return self.parts[key]

            def []=(ref self, key: str, part: Part):
                self.parts[key] = part

        struct Rack:
            counts: list[int]

            def [](self, at: int) -> int:
                return self.counts[at]

            def []=(ref self, at: int, count: int):
                self.counts[at] = count

        def failing(ref value: int, by: int) fails Nope:
            value += by
            fail Nope(code=by)

        def main(stdout: Stdout):
            mut shelf = Shelf(parts={'bolt': Part(name='bolt', x=1, marks=[])})
            shelf['bolt'].shift(2) except why:
                stdout.print('failed', why.code)
            stdout.print(shelf['bolt'])
            shelf['bolt'].shift(9) except why:
                stdout.print('failed', why.code)
            stdout.print(shelf['bolt'])
            mut rack = Rack(counts=[1, 2])
            failing(ref rack[1], by=40) except Nope():
                stdout.print('failed', rack.counts)
            return
        ",
        "Part(name=\"bolt\", x=103, marks=[2])\n\
         failed 9\n\
         Part(name=\"bolt\", x=112, marks=[2, 9])\n\
         failed [1, 42]\n",
    );
}

/// `bytes`: made by `encode` and `bytes(items)`, read by place and by slice, gone through, joined,
/// compared, written as hex and read back from it, and decoded, with the words the runtime fails
/// with. `std.base64` runs from its own source over them, with the intrinsics it calls modeled
/// here. What it prints is what the compiled program printed.
#[test]
fn bytes_are_made_read_joined_and_decoded() {
    prints(
        "
        from std import base64

        def problem(text: str) -> str:
            _ = bytes.fromhex(text) except why:
                return why.message
            return 'read'

        def main(stdout: Stdout):
            data = 'fox é'.encode()
            stdout.print(data, len(data), data[-1], data[1:3], 111 in data, [byte for byte in data[:2]])
            joined = bytes([1, 2]) + bytes([255])
            stdout.print(joined, joined == bytes([1, 2, 255]), bytes() < joined, joined.hex(), joined.hex(':'))
            stdout.print(bytes.fromhex('0a 0B')?, problem('zz'), problem('8'))
            text = bytes([255]).decode() except why:
                why.message
            stdout.print(text, data.decode()?)
            stdout.print(base64.encode(data), base64.decode('Zm94IMOp')? == data)
            return
        ",
        "bytes([102, 111, 120, 32, 195, 169]) 6 169 bytes([111, 120]) True [102, 111]\n\
         bytes([1, 2, 255]) True True 0102ff 01:02:ff\n\
         bytes([10, 11]) `fromhex` needs two hex digits for each byte, but found \"z\" at 0 `fromhex` needs \
         two hex digits for each byte, but the text ends halfway through one\n\
         these bytes aren't UTF-8 text: invalid utf-8 sequence of 1 bytes from index 0 fox é\n\
         Zm94IMOp True\n",
    );
}

/// A template string builds the core's own `Template`, run from its source: its texts, values and
/// expressions, and each `Interpolation` with its format spec. `is bytes` tells bytes from the
/// other members of a union. What it prints is what the compiled program printed.
#[test]
fn a_template_string_builds_the_cores_template() {
    prints(
        "
        type Field = int | bytes

        def kind(value: Field) -> str:
            if value is bytes:
                return 'bytes ' + value.hex()
            return 'int'

        def main(stdout: Stdout):
            row = 7
            blob = bytes([1, 255])
            query: Template[Field] = t'row = {row} and blob = {blob:x}'
            stdout.print(query.strings, len(query.values), query.expressions)
            for part in query.interpolations():
                stdout.print(part.expression, part.format_spec, kind(part.value))
            return
        ",
        "[\"row = \", \" and blob = \", \"\"] 2 [\"row\", \"blob\"]\n\
         row  int\n\
         blob x bytes 01ff\n",
    );
}

/// The program of `a_format_of_its_own_is_told_what_each_value_saves`: a format that writes what
/// it's told, and the values given it.
const OWN_FORMAT: &str = "
        from std.collections import Id

        struct Shown:
            parts = list[str]()

            def int(ref self, value: int):
                self.parts.append(str(value))

            def float(ref self, value: float):
                self.parts.append(str(value))

            def f32(ref self, value: f32):
                self.parts.append(str(value))

            def bool(ref self, value: bool):
                self.parts.append('yes' if value else 'no')

            def text(ref self, value: str):
                self.parts.append(f'<{value}>')

            def bytes(ref self, value: bytes):
                self.parts.append('bytes')

            def none(ref self):
                self.parts.append('nothing')

            def value[T: Serializable](ref self, x: T):
                x.serialize(ref self)

            def sequence[T: Serializable](ref self, items: list[T]):
                self.parts.append('(')
                for item in items:
                    item.serialize(ref self)
                self.parts.append(')')

            def map[K: Serializable, V: Serializable](ref self, pairs: list[(K, V)]):
                for key, value in pairs:
                    key.serialize(ref self)
                    self.parts.append('=')
                    value.serialize(ref self)

            def record(ref self, name: str, count: int):
                self.parts.append(f'{name}/{count}')

            def field[T: Serializable](ref self, name: str, x: T):
                self.parts.append(f'{name}:')
                x.serialize(ref self)

            def renamed[T: Serializable](ref self, name: str, x: T):
                self.parts.append(f'as {name}:')
                x.serialize(ref self)

            def end(ref self):
                self.parts.append('.')

            def items(ref self, name: str, count: int):
                self.parts.append('[')

            def item[T: Serializable](ref self, x: T):
                x.serialize(ref self)

            def end_items(ref self):
                self.parts.append(']')

            def enum_case(ref self, name: str, case_name: str, at: int):
                self.parts.append(f'{name}.{case_name}')

            def case_record(ref self, name: str, case_name: str, at: int, count: int):
                self.parts.append(f'{name}.{case_name}/{count}')

            def member(ref self, name: str, at: int):
                pass

            def end_member(ref self):
                pass

            def table[T: Serializable](ref self, kind: str, ids: list[Id[T]], items: list[T]):
                self.sequence(items)

            def exact(self) -> bool:
                return False

            def slots[T: Serializable](ref self, kind: str, tags: list[int], slots: list[int], items: list[T]):
                # Never asked for: `exact` says no.
                pass

            def id(ref self, kind: str, key: int, tag: int):
                self.parts.append(f'Id[{kind}]')

            def some[T: Serializable](ref self, x: T):
                x.serialize(ref self)

            def shown(self) -> str:
                return ' '.join(self.parts)







        enum Shape:
            Dot
            Box(width: int, height: int)

        struct Pair[T]:
            first: T
            @serialize(name='other')
            second: T
            @skip
            seen = 0

        def shown[T: Serializable](x: T) -> str:
            mut out = Shown()
            out.value(x)
            return out.shown()

        def main(stdout: Stdout):
            stdout.print(shown(Pair(first=Shape.Dot, second=.Box(width=2, height=3))))
            maybes: list[int?] = [1, None]
            stdout.print(shown(maybes), shown({'a': {2}}))
            return
        ";

/// A format a program writes itself is told what each value saves, by the value's type: an
/// optional's value through `some`, a list's items through `sequence`, a struct's fields between
/// `record` and `end`, renamed or left out as their annotations say, and an enum's case through
/// `enum_case` or `case_record`. A generic function's `T` is the type it's called with. What it
/// prints is what the compiled program printed.
#[test]
fn a_format_of_its_own_is_told_what_each_value_saves() {
    prints(
        OWN_FORMAT,
        "Pair/2 first: Shape.Dot as other: Shape.Box/2 width: 2 height: 3 . . .\n\
         ( 1 nothing ) <a> = ( 2 )\n",
    );
}

/// JSON saves and loads a standard library type through the `def serialize` and
/// `def deserialize` it writes itself, run from its source: a `SortedMap` as an object, a
/// `Grid` as its rows, and a failure the type's own code makes says where it is. What it prints is
/// what the compiled program printed.
#[test]
fn json_runs_a_types_own_serialize_and_deserialize() {
    prints(
        "
        from std import json
        from std.collections import Grid, SortedMap

        def failure[T](run: () -> T fails ParseError) -> str:
            _ = run() except why:
                return why.message
            return 'loaded'

        def main(stdout: Stdout):
            mut ranks = SortedMap[int, str]()
            ranks[10] = 'ten'
            ranks[2] = 'two'
            text = json.dumps(ranks)
            stdout.print(text, json.loads[SortedMap[int, str]](text)?.items())
            stdout.print(json.dumps(Grid.of([[1, 2], [3, 4]])))
            stdout.print(failure(() => json.loads[Grid[int]]('[[1, 2], [3]]')?))
            return
        ",
        "{\"2\": \"two\", \"10\": \"ten\"} [(2, \"two\"), (10, \"ten\")]\n\
         [[1, 2], [3, 4]]\n\
         `[1]` has 1 cell, but the first row has 2\n",
    );
}

/// `toml.dumps` is the runtime's, written in Rust, and the oracle runs `std.toml`'s own writer,
/// written in Fennec, in its place, through each value's `serialize`: so the two are held to each
/// other. Floats are written by `std.json`'s helper, run from its source. What it prints is what
/// the compiled program printed.
#[test]
fn toml_dumps_runs_std_tomls_own_writer() {
    prints(
        "
        from std import toml

        struct Limit:
            name: str
            most: float

        struct Server:
            name: str
            port: int
            tags: list[str]
            limits: dict[str, float]
            rules: list[Limit]
            owner: Limit

        def main(stdout: Stdout):
            rules = [Limit(name='cpu', most=0.5), Limit(name='mem \\'hot\\'', most=2.0)]
            server = Server(name='den', port=8080, tags=['a', 'b c'], limits={'cpu': 0.5, 'mem': 2.0}, rules=rules, owner=Limit(name='kit', most=1.0))
            stdout.write(toml.dumps(server))
            stdout.print('---')
            stdout.write(toml.dumps({'title': 'fox \\'kit\\'', 'count': '3'}))
            return
        ",
        "name = \"den\"\n\
         port = 8080\n\
         tags = [\"a\", \"b c\"]\n\
         \n\
         [limits]\n\
         cpu = 0.5\n\
         mem = 2.0\n\
         \n\
         [[rules]]\n\
         name = \"cpu\"\n\
         most = 0.5\n\
         \n\
         [[rules]]\n\
         name = \"mem \\\"hot\\\"\"\n\
         most = 2.0\n\
         \n\
         [owner]\n\
         name = \"kit\"\n\
         most = 1.0\n\
         ---\n\
         title = \"fox \\\"kit\\\"\"\n\
         count = \"3\"\n",
    );
}

/// An `f32` is rounded to 32 bits after each operation and prints as the shortest text that reads
/// back as the same `f32`, as `docs/language.md` says: written out, converted from ints, floats
/// and sized ints, added, multiplied, divided, compared, and back to a `float` and an `int`. What
/// it prints is what the compiled program printed.
#[test]
fn an_f32_is_rounded_after_each_step_and_prints_shortest() {
    prints(
        "
        struct Body:
            mass: f32
            speed: f32

        def main(stdout: Stdout):
            tenth: f32 = 0.1
            three = f32(3)
            summed = tenth + tenth + tenth
            stdout.print(tenth, three, summed, tenth * three, three / f32(7), -tenth, summed == f32(0.3), summed < three)
            stdout.print(float(tenth), int(three / f32(2)), f32(16777217), f32(1e39), f32(2.5e-7), f32(123456789.0))
            body = Body(mass=1.5, speed=f32(1) / f32(3))
            stdout.print(body, [tenth, three], {tenth: 'tenth'}, f'{summed}')
            mut total: f32 = 0.0
            for step in range(10):
                total += f32(step) * tenth
            stdout.print(total, f32(u8(200)), f32(-0.0), f32(1e20))
            return
        ",
        "0.1 3.0 0.3 0.3 0.42857143 -0.1 True True\n\
         0.10000000149011612 1 16777216.0 inf 2.5e-07 123456790.0\n\
         Body(mass=1.5, speed=0.33333334) [0.1, 3.0] {0.1: \"tenth\"} 0.3\n\
         4.5 200.0 -0.0 1e+20\n",
    );
}

/// `std.math`'s `sqrt`, `sin`, `cos` and `acos` take an `f32` and give one, computed in `f32` by
/// `libm`'s own, the same bits everywhere, and `abs` of an `f32` is one. Given a `float`, each is
/// what it was.
#[test]
fn std_math_takes_an_f32_and_gives_one() {
    prints(
        "
        from std import math

        def main(stdout: Stdout):
            half: f32 = 0.5
            root: f32 = math.sqrt(f32(2))
            stdout.print(root, math.sin(half), math.cos(half), math.acos(half), abs(f32(-1.5)))
            stdout.print(root * root, math.sqrt(2.0))
        ",
        "1.4142135 0.47942555 0.87758255 1.0471976 1.5\n1.9999999 1.4142135623730951\n",
    );
}

/// `f32.parse` reads text straight to the nearest `f32`, rounded once. Read as a `float` first and
/// converted, a number just past halfway between two `f32`s lands on halfway and rounds to the
/// even one: this one reads as the `f32` after 1, and through a `float` as 1. What isn't a number
/// fails, saying so.
#[test]
fn f32_parse_rounds_once() {
    prints(
        "
        def main(stdout: Stdout):
            once = f32.parse('1.00000005960464477539062501') except InvalidValue(): f32(0)
            wide = float.parse('1.00000005960464477539062501') except InvalidValue(): 0.0
            twice = f32(wide)
            bad = f32.parse('1,5') except why:
                stdout.print(why)
                f32(0)
            spaced = f32.parse(' -2e3 ') except InvalidValue(): f32(0)
            stdout.print(once, twice, once == twice, bad, spaced)
        ",
        "\"1,5\" isn't a float\n1.0000001 1.0 False 0.0 -2000.0\n",
    );
}

/// `toml.loads` runs `std.toml`'s own reader, written in Fennec, in place of the runtime's,
/// through the `def deserialize` the checker writes for each type, as the oracle models it: a
/// field left out takes its default, an optional one is `None`, a `@skip` field keeps its
/// default, and a missing field, a value of the wrong type, a key given twice and a key no field
/// has each fail as the reader says. What it prints is what the compiled program printed.
#[test]
fn toml_loads_runs_std_tomls_own_reader_through_each_types_deserialize() {
    prints(
        "
        from std import toml

        struct Limit:
            name: str
            most = 1.0

        struct Server:
            name: str
            tags: list[str]
            note: str?
            limits: dict[str, float]
            rules: list[Limit]
            port = 80
            @skip
            seen = 3

        def failure(text: str) -> str:
            _ = toml.loads[Server](text) except why:
                return why.message
            return 'loaded'

        def main(stdout: Stdout):
            text = 'name = \\'den\\'\\ntags = [\\'a\\']\\n\\n[limits]\\ncpu = 0.5\\n\\n[[rules]]\\nname = \\'cpu\\'\\n\\n[[rules]]\\nname = \\'mem\\'\\nmost = 2.5\\n'
            server = toml.loads[Server](text)?
            stdout.print(server)
            again = toml.dumps(Limit(name='kit'))
            stdout.print(again.strip(), toml.loads[Limit](again)? == Limit(name='kit'))
            stdout.print(failure('tags = []\\n[limits]\\n'))
            stdout.print(failure('name = 3\\ntags = []\\n[limits]\\n'))
            stdout.print(failure('name = \\'x\\'\\ntags = []\\nport = 1\\nport = 2\\n[limits]\\n'))
            stdout.print(failure('name = \\'x\\'\\ntags = []\\nextra = 1\\n[limits]\\n'))
            stdout.print(failure('name = \\'x\\'\\nnote = \\'hi\\'\\ntags = [1]\\n[limits]\\n'))
            counts = toml.loads[dict[str, list[int]]]('a = [1, 2]\\nb = []\\n')?
            stdout.print(counts)
            return
        ",
        "Server(name=\"den\", tags=[\"a\"], note=None, limits={\"cpu\": 0.5}, rules=[Limit(name=\"cpu\", most=1.0), Limit(name=\"mem\", most=2.5)], port=80, seen=3)\n\
         name = \"kit\"\n\
         most = 1.0 True\n\
         `name` is missing\n\
         `name` should be a str, but it's an int\n\
         `port` is defined twice, at line 4, column 1\n\
         `Server` has no field \"extra\", but the text has one\n\
         `tags[0]` should be a str, but it's an int\n\
         {\"a\": [1, 2], \"b\": []}\n",
    );
}

/// The sized ints' `parse` and `from` are the core's, written in Fennec, run from their source:
/// a `u64` past the largest int, a sign and the space around it, and each failure as they say
/// it. What it prints is what the compiled program printed.
#[test]
fn sized_parse_and_from_run_the_cores_own_code() {
    prints(
        "
        def parsed(text: str) -> str:
            value = u64.parse(text) except why:
                return why.message
            return f'{value}'

        def small(text: str) -> str:
            value = i8.parse(text) except why:
                return why.message
            return f'{value}'

        def fitted(number: int) -> str:
            value = u16.from(number) except why:
                return why.message
            return f'{value}'

        def main(stdout: Stdout):
            for text in ['0', '18446744073709551615', '18446744073709551616', '-1', ' 42 ', '1_000', 'x']:
                stdout.print(parsed(text))
            for text in ['127', '-128', '128', '-129', '+5', '']:
                stdout.print(small(text))
            for number in [0, 65535, 65536, -1]:
                stdout.print(fitted(number))
            return
        ",
        "0\n\
         18446744073709551615\n\
         18446744073709551616 doesn't fit in u64, which holds 0 to 18446744073709551615\n\
         -1 doesn't fit in u64, which holds 0 to 18446744073709551615\n\
         42\n\
         \"1_000\" isn't an int\n\
         \"x\" isn't an int\n\
         127\n\
         -128\n\
         128 doesn't fit in i8, which holds -128 to 127\n\
         -129 doesn't fit in i8, which holds -128 to 127\n\
         5\n\
         \"\" isn't an int\n\
         0\n\
         65535\n\
         65536 doesn't fit in u16, which holds 0 to 65535\n\
         -1 doesn't fit in u16, which holds 0 to 65535\n",
    );
}

/// Tasks launched with no `as` run beside their block, talking through `std.concurrent`'s
/// channels: a task answering each job its block sends, and two listeners of a channel to every
/// listener, each hearing every message sent from when its inbox was made. What it prints is
/// what the compiled program printed.
#[test]
fn tasks_talk_through_channels_and_listeners() {
    prints(
        "
        from std.concurrent import Closed, Crashed, Inbox, Sender, channel

        def doubled(jobs: Inbox[int], back: Sender[int], count: int) fails Closed:
            for _ in range(count):
                job = jobs.receive() else 0
                back.send(job * 2)?

        def summed(news: Inbox[int], back: Sender[int], count: int) fails Closed:
            mut total = 0
            mut heard = 0
            for item in news:
                total += item
                heard += 1
                if heard == count: break
            back.send(total)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=2) as jobs, channel[int](size=4) as answers:
                with launch(() => doubled(jobs.inbox(), back=answers.sender(), count=5)?)?:
                    for job in range(1, 6):
                        jobs.send(job)?
                        stdout.print(answers.receive() else -1)
            with channel[int](size=8, to=.Every) as news, channel[int](size=2) as totals:
                first = news.inbox()
                second = news.inbox()
                with launch(() => summed(first, back=totals.sender(), count=3)?)?:
                    with launch(() => summed(second, back=totals.sender(), count=3)?)?:
                        for item in [3, 4, 5]:
                            news.send(item)?
                stdout.print(totals.receive() else -1, totals.receive() else -1)
            return
        ",
        "2\n\
         4\n\
         6\n\
         8\n\
         10\n\
         12 12\n",
    );
}

/// A channel to every listener keeps the last `size` messages. A listener whose receive happens
/// after the sends that let what it hadn't read go falls behind on every run: its next receive
/// fails as a crash does, or with `when_behind=.DropOldest` goes on from the oldest kept, as the
/// runtime's does. An inbox made after some were sent hears from there. What it prints is what the
/// compiled program printed, the same each run. Where whether a listener read a message or found it
/// gone hangs on time, the run is refused: its receive not after the send that let it go, or its
/// read not before it, whether the sender came to the send through another task's message or its
/// own timeout.
#[test]
fn a_listener_left_behind_fails_or_skips_and_one_that_could_be_reading_is_refused() {
    prints(BEHIND, "early fell behind\ntook [4, 5]\nskipped to [4, 5]\n");
    let (printed, ended) = ran(RACING);
    let refused = "a listener of a channel to every listener found a message gone that it could have read first, by time";
    assert_eq!(ended, Ended::Unsupported(refused.to_owned()), "{printed}");
    let refused = "a channel to every listener let go of a message a listener read, the read not before the send, so it read it or fell behind by time";
    for read_first in [READ_THEN_SENT_ON_A_GATE, READ_THEN_SENT_AFTER_A_TIMEOUT] {
        let (printed, ended) = ran(read_first);
        assert_eq!(ended, Ended::Unsupported(refused.to_owned()), "{printed}");
    }
}

const READ_THEN_SENT_ON_A_GATE: &str = "
        from std.concurrent import Channel, Closed, Crashed, Inbox, channel

        def took(hits: Inbox[int], stdout: Stdout):
            got = hits.receive() else -1
            stdout.print('took', got)

        def later(hits: Channel[int], gate: Channel[int]) fails Closed:
            _ = gate.receive() else fail Closed()
            hits.send(2)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1, to=.Every) as hits:
                with channel[int](size=1) as gate:
                    inbox = hits.inbox()
                    hits.send(1)?
                    with launch(() => took(inbox, stdout=stdout))?:
                        with launch(() => later(hits, gate=gate)?)?:
                            gate.send(0)?
        ";

const READ_THEN_SENT_AFTER_A_TIMEOUT: &str = "
        from std.concurrent import Channel, Closed, Crashed, Inbox, Timeout, channel

        def took(hits: Inbox[int], stdout: Stdout):
            got = hits.receive() else -1
            stdout.print('took', got)

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1, to=.Every) as hits:
                with channel[int](size=1) as quiet:
                    inbox = hits.inbox()
                    hits.send(1)?
                    with launch(() => took(inbox, stdout=stdout))?:
                        _ = quiet.receive_within(2ms) except Timeout(): -1
                        hits.send(2)?
        ";

/// A loop polling with waits of no time, while a task beside it waits with a timeout, runs: each
/// poll asks who could still send on the channel, and the answer is worked out without going
/// over every message for every task each time (#2131). What it prints is what the compiled
/// program printed.
#[test]
fn a_loop_polling_with_waits_of_no_time_beside_a_waiting_task_runs() {
    prints(POLLING_WITH_NO_TIME, "found 20\n");
}

const POLLING_WITH_NO_TIME: &str = "
        from std.concurrent import Closed, Crashed, Inbox, Sender, Timeout, channel

        def waiting(quiet: Inbox[int], done: Sender[int]) fails Closed:
            _ = quiet.receive_within(10s) except Timeout(): -1
            done.send(0)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1) as quiet, channel[int](size=1) as done:
                with launch(() => waiting(quiet.inbox(), done=done.sender())?)?:
                    with channel[int](size=4) as box:
                        mut found = 0
                        for at in range(2000):
                            if at % 100 == 0:
                                box.send(at)?
                            got = box.receive_within(0ms) except Timeout(): -1
                            if got is not None and got >= 0:
                                found += 1
                        stdout.print('found', found)
                    quiet.send(1)?
                    _ = done.receive() else -1
        ";

/// A listener started in a task beside the block's sends hears them or not by time: each place it
/// could start among them is tried, and here they print differently, so the run is refused. One
/// started before the sends, which its task says before they go, hears them all. A timed wait that
/// times out comes after every other task got as far as it could, so a listener that exists then
/// started before what's sent after, and a handshake repeating its send until it's heard has an
/// end, as the fuzzer's does. An untimed wait says nothing about that: a listener whose task
/// waits beside the sends, untimed, is still tried at each place, and here one way hangs (#2115).
#[test]
fn a_listener_started_beside_a_send_is_tried_at_each_place_it_could_start() {
    prints(LISTENER_STARTED_FIRST, "heard 1 2\n");
    let (printed, ended) = ran(LISTENER_STARTED_BESIDE_SENDS);
    let refused = "what it prints, or how it ends, hangs on the order its tasks' messages come in: line 1 is \"heard []\" in one order and \"heard [2]\" in another";
    assert_eq!(ended, Ended::Unsupported(refused.to_owned()), "{printed}");
    let (printed, ended) = ran(LISTENER_STARTED_BESIDE_SENDS_AFTER_AN_UNTIMED_WAIT);
    let hangs = "every task waits on another, so the program hangs";
    assert_eq!(ended, Ended::Unsupported(hangs.to_owned()), "{printed}");
}

const LISTENER_STARTED_BESIDE_SENDS_AFTER_AN_UNTIMED_WAIT: &str = "
        from std.concurrent import Channel, Closed, Crashed, Sender, channel

        def listening(hits: Channel[int], stdout: Stdout):
            inbox = hits.inbox()
            first = inbox.receive() else -1
            stdout.print('heard', first)

        def signalling(ready: Sender[int]) fails Closed:
            ready.send(0)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=4, to=.Every) as hits, channel[int](size=1) as ready:
                with launch(() => listening(hits, stdout=stdout))?:
                    with launch(() => signalling(ready.sender())?)?:
                        _ = ready.receive() else -1
                        hits.send(1)?
                        hits.send(2)?
        ";

const LISTENER_STARTED_FIRST: &str = "
        from std.concurrent import Channel, Closed, Crashed, Sender, channel

        def listening(hits: Channel[int], ready: Sender[int], stdout: Stdout) fails Closed:
            inbox = hits.inbox()
            ready.send(0)?
            first = inbox.receive() else -1
            second = inbox.receive() else -1
            stdout.print('heard', first, second)

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=4, to=.Every) as hits, channel[int](size=1) as ready:
                with launch(() => listening(hits, ready=ready.sender(), stdout=stdout)?)?:
                    _ = ready.receive() else -1
                    hits.send(1)?
                    hits.send(2)?
        ";

const LISTENER_STARTED_BESIDE_SENDS: &str = "
        from std.concurrent import Channel, Closed, Crashed, Sender, Timeout, channel

        def listening(hits: Channel[int], replies: Sender[int], stdout: Stdout) fails Closed:
            mut got: list[int] = []
            for hit in hits.listen():
                if hit < 0:
                    replies.send(0)?
                    break
                got.append(hit)
            stdout.print('heard', got)

        def main(stdout: Stdout) fails Closed | Crashed | Timeout:
            with channel[int](size=8, to=.Every) as hits, channel[int](size=1) as replies:
                with launch(() => listening(hits, replies=replies.sender(), stdout=stdout)?)?:
                    hits.send(1)?
                    hits.send(2)?
                    while True:
                        hits.send(-1)?
                        heard = replies.receive_within(2ms) except Timeout(): continue
                        if heard is not None:
                            break
        ";

/// A wait of no time looks once, as the runtime's does: what it finds is the same on every run
/// only when nothing else could still send, take or close, and what it finds was there before it.
/// A receive of no time beside a task that sends, which the compiled program finds empty, was read
/// as the task's message; it's refused now, as is a send of no time beside a task that takes. One
/// with nothing beside it prints what the compiled program printed. One listener read by two tasks
/// at once is refused as one channel's receivers are, which gets which hanging on time (#2116).
/// Refused too, from the review: a send of no time to a channel a task beside it filled, one in
/// a `parallel` round beside another round that takes, and a receive of no time while a task waits
/// for room behind a send beside its own, which on another run got in first and went on, the send
/// a task's or a `parallel` round's. A round's wait of no time beside other rounds is refused on a
/// channel it made itself too, since its ends are plain values another round can be sent.
/// A task that doesn't know a channel was made holds no end of it, so std's `pool` runs; but one
/// whose `parallel` round took an end, on its thread or one it handed rounds to, or that could take
/// one handed on through two channels, does, and a task that knows nothing of it but could wake
/// one blocked that holds an end counts too.
#[test]
fn a_wait_of_no_time_or_a_listener_read_by_two_tasks_is_refused_where_it_hangs_on_time() {
    prints(WAITS_OF_NO_TIME, "got 5\nagain -1\nfull\n");
    for (program, refused) in [
        (RECEIVE_OF_NO_TIME_BESIDE_A_SENDER, "a receive of no time, which finds a message or not by time"),
        (SEND_OF_NO_TIME_BESIDE_A_TAKER, "a send of no time, which finds room or not by time"),
        (SEND_OF_NO_TIME_TO_ONE_FILLED_BESIDE, "a send of no time, which finds room or not by time"),
        (SEND_OF_NO_TIME_IN_A_ROUND, "a send of no time, which finds room or not by time"),
        (
            RECEIVE_OF_NO_TIME_BEHIND_A_RACING_SEND,
            "a receive of no time, which finds a message or not by time",
        ),
        (
            RECEIVE_OF_NO_TIME_BEHIND_A_RACING_SEND_IN_A_ROUND,
            "a receive of no time, which finds a message or not by time",
        ),
        (
            WAITS_OF_NO_TIME_ON_A_ROUND_S_OWN_CHANNEL,
            "a receive of no time, which finds a message or not by time",
        ),
        (
            RECEIVE_OF_NO_TIME_ON_AN_END_A_ROUND_TOOK,
            "a receive of no time, which finds a message or not by time",
        ),
        (
            RECEIVE_OF_NO_TIME_ON_AN_END_HANDED_TWICE,
            "a receive of no time, which finds a message or not by time",
        ),
        (
            RECEIVE_OF_NO_TIME_WHILE_A_HOLDER_COULD_BE_WOKEN,
            "a receive of no time, which finds a message or not by time",
        ),
        (
            RECEIVE_OF_NO_TIME_ON_AN_END_A_HANDED_ROUND_TOOK,
            "a receive of no time, which finds a message or not by time",
        ),
        (
            ONE_LISTENER_TWO_READERS,
            "two tasks taking messages from one listener of a channel to every listener, which gets which hanging on time",
        ),
    ] {
        let (printed, ended) = ran(program);
        assert_eq!(ended, Ended::Unsupported(refused.to_owned()), "{printed}");
    }
}

const WAITS_OF_NO_TIME: &str = "
        from std.concurrent import Closed, Timeout, channel

        def main(stdout: Stdout) fails Closed:
            with channel[int](size=1) as box:
                box.send(5)?
                got = box.receive_within(0ms) except Timeout(): -1
                stdout.print('got', got)
                again = box.receive_within(0ms) except Timeout(): -1
                stdout.print('again', again)
                box.send(6)?
                box.send_within(7, 0ms) except why:
                    case Timeout():
                        stdout.print('full')
                        return
                    case _:
                        stdout.print('closed', why)
                        return
                stdout.print('sent')
        ";

const RECEIVE_OF_NO_TIME_BESIDE_A_SENDER: &str = "
        from std.concurrent import Closed, Crashed, Sender, Timeout, channel

        def sending(box: Sender[int]) fails Closed:
            box.send(5)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1) as box:
                with launch(() => sending(box.sender())?)?:
                    got = box.receive_within(0ms) except Timeout(): -1
                    stdout.print('got', got)
        ";

const SEND_OF_NO_TIME_BESIDE_A_TAKER: &str = "
        from std.concurrent import Closed, Crashed, Inbox, Timeout, channel

        def taking(box: Inbox[int]):
            _ = box.receive() else -1

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1) as box:
                box.send(5)?
                with launch(() => taking(box.inbox())):
                    box.send_within(6, 0ms) except why:
                        case Timeout():
                            stdout.print('full')
                            return
                        case _:
                            stdout.print('closed', why)
                            return
                    stdout.print('sent')
        ";

const ONE_LISTENER_TWO_READERS: &str = "
        from std.concurrent import Closed, Crashed, Inbox, Sender, channel

        def reading(hits: Inbox[int], back: Sender[int]) fails Closed:
            got = hits.receive() else -1
            back.send(got)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=4, to=.Every) as hits, channel[int](size=1) as first, channel[int](size=1) as second:
                inbox = hits.inbox()
                with launch(() => reading(inbox, back=first.sender())?)?:
                    with launch(() => reading(inbox, back=second.sender())?)?:
                        hits.send(1)?
                        hits.send(2)?
                stdout.print('a', first.receive() else -1, 'b', second.receive() else -1)
        ";

const RECEIVE_OF_NO_TIME_WHILE_A_HOLDER_COULD_BE_WOKEN: &str = "
        from std.concurrent import Closed, Crashed, Inbox, Sender, Timeout, channel

        def holder(pipe: Inbox[Sender[int]], ack: Sender[int], wake: Inbox[int], done: Sender[int]) fails Closed:
            out = pipe.receive() else return
            ack.send(0)?
            _ = wake.receive() else -1
            out.send(7)?
            done.send(0)?

        def waker(quiet: Inbox[int], wake: Sender[int]) fails Closed:
            _ = quiet.receive_within(1ns) except Timeout(): -1
            wake.send(0)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[Sender[int]](size=1) as pipe, channel[int](size=1) as ack, channel[int](size=1) as wake:
                with channel[int](size=1) as quiet, channel[int](size=1) as done:
                    with launch(() => waker(quiet.inbox(), wake=wake.sender())?)?:
                        with launch(() => holder(pipe.inbox(), ack=ack.sender(), wake=wake.inbox(), done=done.sender())?)?:
                            with channel[int](size=1) as box:
                                pipe.send(box.sender())?
                                _ = ack.receive() else -1
                                got = box.receive_within(0ms) except Timeout(): -1
                                stdout.print('got', got)
                                _ = done.receive() else -1
        ";

const RECEIVE_OF_NO_TIME_ON_AN_END_A_HANDED_ROUND_TOOK: &str = "
        from std.concurrent import Closed, Crashed, Inbox, Sender, Timeout, channel

        def take(pipe: Inbox[Sender[int]], ack: Sender[int], go: Inbox[int], at: int) -> list[Sender[int]] fails Closed:
            if at == 0:
                _ = go.receive() else -1
                return []
            out = pipe.receive() else return []
            ack.send(0)?
            return [out]

        def worker(pipe: Inbox[Sender[int]], ack: Sender[int], go: Inbox[int], done: Sender[int]) fails Closed:
            taken = [take(pipe, ack=ack, go=go, at=at)? for at in parallel(range(2))]
            for outs in taken:
                for out in outs:
                    out.send(7)?
            done.send(0)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[Sender[int]](size=1) as pipe, channel[int](size=1) as ack, channel[int](size=1) as go:
                with channel[int](size=1) as done:
                    go.send(0)?
                    with launch(() => worker(pipe.inbox(), ack=ack.sender(), go=go.inbox(), done=done.sender())?)?:
                        with channel[int](size=1) as box:
                            pipe.send(box.sender())?
                            _ = ack.receive() else -1
                            got = box.receive_within(0ms) except Timeout(): -1
                            stdout.print('got', got)
                            _ = done.receive() else -1
        ";

const RECEIVE_OF_NO_TIME_ON_AN_END_A_ROUND_TOOK: &str = "
        from std.concurrent import Closed, Crashed, Inbox, Sender, Timeout, channel

        def relay(pipe: Inbox[Sender[int]], ack: Sender[int], gate: Inbox[int], done: Sender[int], at: int) -> int fails Closed:
            out = pipe.receive() else return -1
            ack.send(0)?
            _ = gate.receive() else -1
            out.send(7)?
            done.send(0)?
            return at

        def worker(pipe: Inbox[Sender[int]], ack: Sender[int], gate: Inbox[int], done: Sender[int]) fails Closed:
            _ = [relay(pipe, ack=ack, gate=gate, done=done, at=at)? for at in parallel(range(1))]

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[Sender[int]](size=1) as pipe, channel[int](size=1) as ack:
                with channel[int](size=1) as gate, channel[int](size=1) as done:
                    gate.send(0)?
                    with launch(() => worker(pipe.inbox(), ack=ack.sender(), gate=gate.inbox(), done=done.sender())?)?:
                        with channel[int](size=1) as box:
                            pipe.send(box.sender())?
                            _ = ack.receive() else -1
                            got = box.receive_within(0ms) except Timeout(): -1
                            stdout.print('got', got)
                            _ = done.receive() else -1
        ";

const RECEIVE_OF_NO_TIME_ON_AN_END_HANDED_TWICE: &str = "
        from std.concurrent import Closed, Crashed, Inbox, Sender, Timeout, channel

        def relay(first: Inbox[Inbox[Sender[int]]], done: Sender[int]) fails Closed:
            second = first.receive() else return
            out = second.receive() else return
            out.send(7)?
            done.send(0)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[Inbox[Sender[int]]](size=1) as first, channel[int](size=1) as done:
                with launch(() => relay(first.inbox(), done=done.sender())?)?:
                    with channel[Sender[int]](size=1) as second:
                        first.send(second.inbox())?
                        with channel[int](size=1) as box:
                            second.send(box.sender())?
                            got = box.receive_within(0ms) except Timeout(): -1
                            stdout.print('got', got)
                            _ = done.receive() else -1
        ";

const SEND_OF_NO_TIME_TO_ONE_FILLED_BESIDE: &str = "
        from std.concurrent import Closed, Crashed, Sender, Timeout, channel

        def filling(box: Sender[int]) fails Closed:
            box.send(5)?

        def signalling(ready: Sender[int]) fails Closed:
            ready.send(1)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1) as box, channel[int](size=1) as ready:
                with launch(() => filling(box.sender())?)?:
                    with launch(() => signalling(ready.sender())?)?:
                        _ = ready.receive() else -1
                        box.send_within(6, 0ms) except why:
                            case Timeout():
                                stdout.print('full')
                                return
                            case _:
                                stdout.print('closed', why)
                                return
                        stdout.print('sent')
        ";

const SEND_OF_NO_TIME_IN_A_ROUND: &str = "
        from std.concurrent import Channel, Closed, Timeout, channel

        def turn(box: Channel[int], at: int) -> int fails Closed:
            if at == 0:
                box.send_within(6, 0ms) except why:
                    case Timeout():
                        return -1
                    case _:
                        fail why
                return 6
            return box.receive() else -1

        def rounds(box: Channel[int]) -> list[int] fails Closed:
            return [turn(box, at)? for at in parallel(range(2))]

        def main(stdout: Stdout) fails Closed:
            with channel[int](size=1) as box:
                box.send(5)?
                stdout.print(rounds(box)?)
        ";

const RECEIVE_OF_NO_TIME_BEHIND_A_RACING_SEND: &str = "
        from std.concurrent import Closed, Crashed, Sender, Timeout, channel

        def first_in(ready: Sender[int], spot: Sender[int]) fails Closed:
            ready.send(0)?
            spot.send(1)?

        def second_in(spot: Sender[int], box: Sender[int]) fails Closed:
            spot.send(1)?
            box.send(9)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1) as ready, channel[int](size=1) as spot, channel[int](size=1) as box:
                with launch(() => first_in(ready.sender(), spot=spot.sender())?)?:
                    with launch(() => second_in(spot.sender(), box=box.sender())?)?:
                        _ = ready.receive() else -1
                        got = box.receive_within(0ms) except Timeout(): -1
                        stdout.print('got', got)
                        _ = spot.receive() else -1
                        _ = spot.receive() else -1
                        _ = box.receive() else -1
        ";

const RECEIVE_OF_NO_TIME_BEHIND_A_RACING_SEND_IN_A_ROUND: &str = "
        from std.concurrent import Closed, Crashed, Sender, Timeout, channel

        def first_in(ready: Sender[int], spot: Sender[int]) fails Closed:
            ready.send(0)?
            spot.send(1)?

        def pass_on(spot: Sender[int], box: Sender[int], at: int) -> int fails Closed:
            spot.send(1)?
            box.send(9)?
            return at

        def second_in(spot: Sender[int], box: Sender[int]) fails Closed:
            _ = [pass_on(spot, box=box, at=at)? for at in parallel(range(1))]

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1) as ready, channel[int](size=1) as spot, channel[int](size=1) as box:
                with launch(() => first_in(ready.sender(), spot=spot.sender())?)?:
                    with launch(() => second_in(spot.sender(), box=box.sender())?)?:
                        _ = ready.receive() else -1
                        got = box.receive_within(0ms) except Timeout(): -1
                        stdout.print('got', got)
                        _ = spot.receive() else -1
                        _ = spot.receive() else -1
                        _ = box.receive() else -1
        ";

const WAITS_OF_NO_TIME_ON_A_ROUND_S_OWN_CHANNEL: &str = "
        from std.concurrent import Closed, Timeout, channel

        def own(at: int) -> int fails Closed:
            with channel[int](size=1) as box:
                box.send(at)?
                got = box.receive_within(0ms) except Timeout(): -1
                again = box.receive_within(0ms) except Timeout(): -1
                return (got else -2) * 10 + (again else -2)

        def rounds() -> list[int] fails Closed:
            return [own(at)? for at in parallel(range(3))]

        def main(stdout: Stdout) fails Closed:
            stdout.print(rounds()?)
        ";

const BEHIND: &str = "
        from std.concurrent import Channel, Closed, Crashed, Inbox, Sender, Timeout, channel

        def took(hits: Inbox[int], count: int, stdout: Stdout):
            mut got: list[int] = []
            for _ in range(count):
                got.append(hits.receive() else -1)
            stdout.print('took', got)

        def reading(hits: Inbox[int], count: int, stdout: Stdout) fails Crashed:
            with launch(() => took(hits, count=count, stdout=stdout)):
                pass

        def skipping(hits: Channel[int], replies: Sender[int], gate: Channel[int], stdout: Stdout) fails Closed:
            mut waited = False
            mut got: list[int] = []
            for hit in hits.listen(when_behind=.DropOldest):
                if hit < 0:
                    if not waited:
                        replies.send(-1)?
                        waited = True
                        _ = gate.receive() else fail Closed()
                    continue
                got.append(hit)
                if hit == 5:
                    break
            stdout.print('skipped to', got)
            replies.send(0)?

        def until_heard(hits: Channel[int], replies: Channel[int]) fails Closed | Timeout:
            while True:
                hits.send(-1)?
                heard = replies.receive_within(2ms) except Timeout(): continue
                if heard is not None:
                    return

        def inboxes(stdout: Stdout) fails Closed:
            with channel[int](size=2, to=.Every) as hits:
                early = hits.inbox()
                for hit in range(1, 4):
                    hits.send(hit)?
                late = hits.inbox()
                hits.send(4)?
                reading(early, count=2, stdout=stdout) except Crashed():
                    stdout.print('early fell behind')
                hits.send(5)?
                reading(late, count=2, stdout=stdout) except Crashed():
                    stdout.print('late fell behind')

        def dropping(stdout: Stdout) fails Closed | Crashed | Timeout:
            with channel[int](size=2, to=.Every) as hits:
                with channel[int](size=4) as replies:
                    with channel[int](size=1) as gate:
                        with launch(() => skipping(hits, replies=replies.sender(), gate=gate, stdout=stdout)?)?:
                            until_heard(hits, replies=replies)?
                            for hit in range(1, 6):
                                hits.send(hit)?
                            gate.send(1)?
                            _ = replies.receive() else fail Closed()

        def main(stdout: Stdout) fails Closed | Crashed | Timeout:
            inboxes(stdout)?
            dropping(stdout)?
        ";

const RACING: &str = "
        from std.concurrent import Closed, Crashed, Inbox, channel

        def took(hits: Inbox[int], count: int, stdout: Stdout):
            mut got: list[int] = []
            for _ in range(count):
                got.append(hits.receive() else -1)
            stdout.print('took', got)

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=1, to=.Every) as hits:
                inbox = hits.inbox()
                with launch(() => took(inbox, count=2, stdout=stdout))?:
                    hits.send(1)?
                    hits.send(2)?
        ";

/// A task that crashes cuts short every wait its block is in, as a closed channel does, so a
/// receive in the block, itself or in each of its `parallel` pieces, gives `None` and the block
/// reaches its end, where the crash comes out. A wait the block starts after the crash is cut
/// short too. The same from main and from a task. What it prints is what the compiled program
/// printed (`workers/crash_while_pieces_wait.fen`).
#[test]
fn a_crashing_task_cuts_short_every_wait_its_block_is_in() {
    let (printed, ended) =
        ran(include_str!("../../fennec/tests/fixtures/workers/crash_while_pieces_wait.fen"));
    assert_eq!(ended, Ended::Exited(0), "{printed}");
    assert_eq!(printed, include_str!("../../fennec/tests/fixtures/workers/crash_while_pieces_wait.out"));
    prints(
        "
        from std.concurrent import Channel, Crashed, channel

        def child(clock: Clock):
            clock.sleep(20ms)
            nothing: list[int] = []
            _ = nothing[3]

        def both(between: Channel[int], clock: Clock, stdout: Stdout) fails Crashed:
            with launch(() => child(clock))?:
                stdout.print('block gave', between.receive() else -1)

        def main(clock: Clock, stdout: Stdout):
            with channel[int](size=1) as between:
                both(between, clock=clock, stdout=stdout) except why:
                    stdout.print(f'{why}')
            stdout.print('done')
        ",
        "block gave -1\nthe task crashed\ndone\n",
    );
    // A send waiting for room is cut short as a closed channel's: it fails with `Closed`.
    prints(
        "
        from std.concurrent import Channel, Closed, Crashed, channel

        def child(clock: Clock):
            clock.sleep(20ms)
            nothing: list[int] = []
            _ = nothing[3]

        def both(between: Channel[int], clock: Clock, stdout: Stdout) fails Crashed:
            with launch(() => child(clock))?:
                between.send(1) except Closed():
                    stdout.print('first send closed')
                between.send(2) except Closed():
                    stdout.print('second send closed')
                stdout.print('block ends')

        def main(clock: Clock, stdout: Stdout):
            with channel[int](size=1) as between:
                both(between, clock=clock, stdout=stdout) except why:
                    stdout.print(f'{why}')
            stdout.print('done')
        ",
        "second send closed\nblock ends\nthe task crashed\ndone\n",
    );
}

/// A task that fails cuts short every wait its block is in, as a crash does, as the runtime has
/// it: its launcher, waiting for a reply it will never send while still holding a sender of the
/// replies itself, takes `None`, and the failure comes out where the block ends. A send waiting
/// for room fails with `Closed`. The oracle cut short waits only for a crash, and refused these as
/// hangs, as it refused 2 of 200 random programs from `fennec-fuzz`. What it prints is what the compiled
/// program printed (`workers/failed_while_its_launcher_waits.fen`).
#[test]
fn a_failing_task_cuts_short_every_wait_its_block_is_in() {
    let (printed, ended) =
        ran(include_str!("../../fennec/tests/fixtures/workers/failed_while_its_launcher_waits.fen"));
    assert_eq!(ended, Ended::Exited(0), "{printed}");
    assert_eq!(
        printed,
        include_str!("../../fennec/tests/fixtures/workers/failed_while_its_launcher_waits.out")
    );
}

/// A task that ends with messages left in its inbox: whether its launcher's sends of them came
/// before it ended, and were left, or after, and failed with `Closed`, hangs on time, so each is
/// tried, on a run of its own, and the runs have to end the same. A task failing on the first of
/// them ends its block with its failure either way, as the compiled program has it
/// (`workers/task_failed_with_messages_left.fen`). A task returning while sends are still to come,
/// and a block printing after a send that could have failed, end otherwise by time, and are
/// refused.
#[test]
fn a_task_ending_with_messages_left_runs_where_every_order_ends_the_same() {
    let (printed, ended) =
        ran(include_str!("../../fennec/tests/fixtures/workers/task_failed_with_messages_left.fen"));
    assert_eq!(ended, Ended::Exited(0), "{printed}");
    assert_eq!(
        printed,
        include_str!("../../fennec/tests/fixtures/workers/task_failed_with_messages_left.out")
    );
    let (_, ended) = ran("
        from std.concurrent import Closed, Crashed, Inbox

        def first_only(inbox: Inbox[int], stdout: Stdout):
            for number in inbox:
                stdout.print('task', number)
                return

        def sent_all(items: list[int], stdout: Stdout) fails Closed | Crashed:
            with launch((inbox) => first_only(inbox, stdout))? as sends:
                for item in items:
                    sends.send(item)?

        def main(stdout: Stdout):
            sent_all([1, 2, 3], stdout) except why:
                stdout.print(f'{why}')
        ");
    assert_eq!(
        ended,
        Ended::Unsupported("a `with launch` block that fails, which cuts its task short at once".to_owned())
    );
    let (_, ended) = ran("
        from std.concurrent import Closed, Crashed, Inbox

        error Refused:
            code: int

        def guarding(inbox: Inbox[int]) fails Refused:
            for number in inbox:
                if number == 7:
                    fail Refused(code=number)

        def guard_all(items: list[int], stdout: Stdout) fails Closed | Crashed | Refused:
            with launch((inbox) => guarding(inbox)?)? as sends:
                for item in items:
                    sends.send(item)?
                    stdout.print('sent', item)

        def main(stdout: Stdout):
            guard_all([7, 6], stdout) except why:
                stdout.print(f'{why}')
        ");
    let Ended::Unsupported(why) = ended else { panic!("it ended {ended:?}") };
    assert!(why.contains("line 2 is \"sent 6\" in one order and \"Refused(code=7)\" in another"), "{why}");
}

/// Where which of several tasks' messages comes first changes what's printed, every order is
/// tried, and the program is refused: what it prints hangs on time, from the line it parts at.
#[test]
fn messages_whose_order_changes_what_prints_are_refused() {
    let (_, ended) = ran("
        from std.concurrent import Closed, Crashed, Sender, channel

        def told(back: Sender[int], what: int) fails Closed:
            back.send(what)?

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=4) as answers:
                with launch(() => told(answers.sender(), what=1)?)?:
                    with launch(() => told(answers.sender(), what=2)?)?:
                        stdout.print(answers.receive() else -1)
                        stdout.print(answers.receive() else -1)
            return
        ");
    assert_eq!(
        ended,
        Ended::Unsupported(
            "what it prints, or how it ends, hangs on the order its tasks' messages come in: \
             line 1 is \"1\" in one order and \"2\" in another"
                .to_owned()
        )
    );
}

/// The rounds of a `parallel` call run beside each other in the compiled program, so which
/// round's message comes first hangs on time, as two tasks' does: every order is tried, and where
/// what's printed changes, the program is refused. Where it doesn't, it runs, nested calls too.
#[test]
fn each_parallel_round_is_a_sender_of_its_own() {
    let (_, ended) = ran("
        from std.concurrent import Channel, channel

        def piece(number: int, out: Channel[int]) -> int:
            out.send(number) except why:
                return -len(f'{why}')
            return number

        def main(stdout: Stdout):
            with channel[int](size=4) as between:
                made = [piece(number, out=between) for number in parallel(range(2))]
                first = between.receive() else -1
                second = between.receive() else -1
                stdout.print(made, first, second)
        ");
    assert_eq!(
        ended,
        Ended::Unsupported(
            "what it prints, or how it ends, hangs on the order its tasks' messages come in: \
             line 1 is \"[0, 1] 0 1\" in one order and \"[0, 1] 1 0\" in another"
                .to_owned()
        )
    );
    prints(
        "
        from std.concurrent import Channel, channel

        def inner(number: int, out: Channel[int]) -> int:
            out.send(number) except why:
                return -len(f'{why}')
            return number

        def outer(number: int, out: Channel[int]) -> int:
            made = [inner(number * 10 + at, out=out) for at in parallel(range(2))]
            return sum(made)

        def main(stdout: Stdout):
            with channel[int](size=8) as between:
                made = [outer(number, out=between) for number in parallel(range(2))]
                mut total = 0
                for _ in range(4):
                    total += between.receive() else -1
                stdout.print(made, total)
        ",
        "[1, 21] 22\n",
    );
}

/// A round that receives, from a channel made outside it, hands the rounds after it to a thread
/// of their own, as they run beside it in the compiled program: so their messages are there
/// before it takes the next, and it can wait on one of them, in a comprehension, a chain of them,
/// a `for` loop or a nested call. Each output is what the compiled program printed.
#[test]
fn a_round_that_waits_lets_the_rounds_after_it_run() {
    // `workers/task_comprehensions.fen`'s first pieces: one waits for what the other sends.
    prints(
        "
        from std.concurrent import Channel, channel

        def piece(number: int, between: Channel[int]) -> int:
            if number == 0: return between.receive() else -1
            between.send(7) except why:
                return -len(f'{why}')
            return 0

        def main(stdout: Stdout):
            with channel[int](size=1) as between:
                stdout.print([piece(number, between=between) for number in parallel(range(2))])
        ",
        "[7, 0]\n",
    );
    prints(
        "
        from std.concurrent import Channel, channel

        def link(number: int, first: Channel[int], second: Channel[int]) -> int:
            if number == 0: return first.receive() else -1
            if number == 1:
                got = second.receive() else -1
                first.send(got * 2) except why:
                    return -len(f'{why}')
                return got
            second.send(21) except why:
                return -len(f'{why}')
            return 0

        def main(stdout: Stdout):
            with channel[int](size=1) as first:
                with channel[int](size=1) as second:
                    stdout.print([link(number, first=first, second=second) for number in parallel(range(3))])
        ",
        "[42, 21, 0]\n",
    );
    prints(
        "
        from std.concurrent import Channel, channel

        def main(stdout: Stdout):
            with channel[int](size=1) as between:
                with channel[int](size=4) as out:
                    for number in parallel(range(2)):
                        if number == 0:
                            got = between.receive() else -1
                            out.send(got) except why:
                                stdout.print(f'{why}')
                        else:
                            between.send(5) except why:
                                stdout.print(f'{why}')
                    stdout.print(out.receive() else -1)
        ",
        "5\n",
    );
    prints(
        "
        from std.concurrent import Channel, channel

        def inner(at: int, number: int, between: Channel[int]) -> int:
            if number == 0 and at == 0: return between.receive() else -1
            if number == 1 and at == 1:
                between.send(9) except why:
                    return -len(f'{why}')
            return 0

        def outer(number: int, between: Channel[int]) -> list[int]:
            return [inner(at, number=number, between=between) for at in parallel(range(2))]

        def main(stdout: Stdout):
            with channel[int](size=1) as between:
                stdout.print([outer(number, between=between) for number in parallel(range(2))])
        ",
        "[[9, 0], [0, 0]]\n",
    );
}

/// Of rounds that run beside each other, refused: two taking from one channel, which gets which
/// hanging on time; one that waits for what no round sends, a deadlock, which stops the program
/// as compiled code does; and the round that
/// handed the rest on stopping the call, as the rounds after it ran where they may not have.
#[test]
fn rounds_beside_each_other_are_refused_where_time_decides() {
    let (_, ended) = ran("
        from std.concurrent import Channel, Closed, channel

        def piece(number: int, jobs: Channel[int]) -> int:
            return (jobs.receive() else -1) * 10 + number

        def main(stdout: Stdout) fails Closed:
            with channel[int](size=4) as jobs:
                for job in range(1, 4):
                    jobs.send(job)?
                made = [piece(number, jobs=jobs) for number in parallel(range(2))]
                stdout.print(made)
            return
        ");
    assert_eq!(
        ended,
        Ended::Unsupported(
            "two tasks taking messages from one channel, which gets which hanging on time".to_owned()
        )
    );
    let (_, ended) = ran("
        from std.concurrent import Channel, channel

        def piece(number: int, between: Channel[int]) -> int:
            return between.receive() else -1

        def main(stdout: Stdout):
            with channel[int](size=1) as between:
                stdout.print([piece(number, between=between) for number in parallel(range(1))])
        ");
    assert_eq!(ended, Ended::Stopped(DEADLOCK.to_owned()));
    let (_, ended) = ran("
        from std.concurrent import Channel, channel

        error Odd:
            at: int

        def piece(number: int, between: Channel[int]) -> int fails Odd:
            if number == 0:
                got = between.receive() else -1
                fail Odd(at=got)
            between.send(3) except why:
                return -len(f'{why}')
            return number

        def run(between: Channel[int]) -> int fails Odd:
            made = [piece(number, between=between)? for number in parallel(range(3))]
            return len(made)

        def main(stdout: Stdout):
            with channel[int](size=1) as between:
                got = run(between) except why:
                    stdout.print(f'{why}')
                    return
                stdout.print(got)
        ");
    assert_eq!(
        ended,
        Ended::Unsupported(
            "a `parallel` round that stops the call after the rounds after it ran beside it".to_owned()
        )
    );
}

/// A `shared` value a round makes is its own, which no other round reaches, until it's sent on a
/// channel: then another round may have it, and two locking it to change it, in an order time
/// decides, are refused. Here round 0 changes it after sending it and round 1 after taking it,
/// so the compiled program can print `[12, 0]` or `[21, 0]`.
#[test]
fn a_shared_value_sent_from_its_round_is_no_longer_its_own() {
    let (_, ended) = ran("
        from std.concurrent import Channel, channel

        struct Count:
            total = 0

        struct Holder:
            count: shared Count

        def piece(number: int, between: Channel[Holder], back: Channel[int]) -> int:
            if number == 0:
                shared mine = Count()
                between.send(Holder(count=mine)) except why:
                    return -len(f'{why}')
                lock mine as ref counting:
                    counting.total = counting.total * 10 + 1
                _ = back.receive() else -1
                lock mine as seen:
                    return seen.total
            shared spare = Count()
            got = between.receive() else Holder(count=spare)
            lock got.count as ref counting:
                counting.total = counting.total * 10 + 2
            back.send(0) except why:
                return -len(f'{why}')
            return 0

        def main(stdout: Stdout):
            with channel[Holder](size=1) as between:
                with channel[int](size=1) as back:
                    stdout.print([piece(number, between=between, back=back) for number in parallel(range(2))])
        ");
    assert_eq!(
        ended,
        Ended::Unsupported(
            "a `shared` value one `parallel` round changes and another reaches, in an order that isn't promised"
                .to_owned()
        )
    );
}

/// Taking from a channel in turn, where each knows the last take happened, is in order, and so is
/// taking from a round's own channel, which no other round reaches: both run.
#[test]
fn taking_from_a_channel_in_turn_or_from_a_rounds_own_runs() {
    prints(
        "
        from std.concurrent import Channel, channel

        def inner(number: int, out: Channel[int]) -> int:
            out.send(number) except why:
                return -len(f'{why}')
            return number

        def piece(number: int) -> int:
            with channel[int](size=4) as mine:
                made = [inner(number * 10 + at, out=mine) for at in parallel(range(2))]
                mut total = 0
                for _ in made:
                    total += mine.receive() else -1
                return total

        def main(stdout: Stdout):
            stdout.print([piece(number) for number in parallel(range(3))])
        ",
        "[1, 21, 41]\n",
    );
    prints(
        "
        from std.concurrent import Channel, Closed, channel

        def piece(number: int, jobs: Channel[int]) -> int:
            return (jobs.receive() else -1) * 10 + number

        def main(stdout: Stdout) fails Closed:
            with channel[int](size=4) as jobs:
                for job in range(1, 4):
                    jobs.send(job)?
                first = jobs.receive() else -1
                made = [piece(number, jobs=jobs) for number in parallel(range(1))]
                last = jobs.receive() else -1
                stdout.print(first, made, last)
            return
        ",
        "1 [20] 3\n",
    );
}

/// A round after one that failed may not run at all, so a message it sends may never be sent:
/// it's refused, as its changing a `shared` value is.
#[test]
fn a_parallel_round_that_may_not_run_using_a_channel_is_refused() {
    let (_, ended) = ran("
        from std.concurrent import Channel, channel

        error Odd:
            at: int

        def piece(number: int, out: Channel[int]) -> int fails Odd:
            if number == 0: fail Odd(at=number)
            out.send(number) except why:
                return -len(f'{why}')
            return number

        def made(out: Channel[int]) -> int fails Odd:
            pieces = [piece(number, out=out)? for number in parallel(range(2))]
            return len(pieces)

        def tried(out: Channel[int]) -> str:
            count = made(out) except why:
                return f'{why}'
            return f'{count}'

        def main(stdout: Stdout):
            with channel[int](size=4) as between:
                stdout.print(tried(between))
                got = between.receive_within(0ms) except why:
                    stdout.print(f'{why}')
                    return
                stdout.print(got else -1)
        ");
    assert_eq!(ended, Ended::Unsupported("a `parallel` round that may not run using a channel".to_owned()));
}

/// A task waiting for its inbox to close, which closes only once the task's own `with launch`
/// has ended, waits forever, as its block does for it: a deadlock, which the runtime finds and
/// stops the program on, with exit code 1, as compiled code does.
#[test]
fn tasks_that_all_wait_stop_the_program_as_a_deadlock() {
    let (_, ended) = ran("
        from std.concurrent import Closed, Crashed, Inbox, channel

        def drained(jobs: Inbox[int]) -> int:
            mut total = 0
            for job in jobs:
                total += job
            return total

        def main(stdout: Stdout) fails Closed | Crashed:
            with channel[int](size=2) as jobs:
                with launch(() => stdout.print(drained(jobs.inbox())))?:
                    jobs.send(1)?
            return
        ");
    assert_eq!(ended, Ended::Stopped(DEADLOCK.to_owned()));
}

/// What the oracle says as a deadlock stops the program, after what it printed.
const DEADLOCK: &str = "error: a deadlock: every task is waiting for another, and nothing can wake them\n";

/// Naming one `shared` value twice in one `lock`, as two lookups of one key do, is a bug: the
/// program stops with exit code 1, saying so, after what it printed, as compiled code does. Two
/// keys are two values, locked together.
#[test]
fn a_lock_taking_one_shared_value_twice_stops_the_program() {
    let (printed, ended) = ran("
        error Short:
            left: int

        struct Account:
            total: int

        def moved(accounts: dict[str, shared Account], from_name: str, to_name: str, amount: int) fails Short:
            lock accounts[from_name], accounts[to_name] as ref source, ref target:
                source.total -= amount
                target.total += amount
                if source.total < 0:
                    fail Short(left=source.total)

        def main(stdout: Stdout):
            shared a = Account(total=10)
            shared b = Account(total=0)
            accounts = {'a': a, 'b': b}
            moved(accounts, from_name='a', to_name='b', amount=4) except why:
                stdout.print('failed', why)
            stdout.print('moved')
            moved(accounts, from_name='a', to_name='a', amount=1) except why:
                stdout.print('failed', why)
            stdout.print('after')
            return
        ");
    assert_eq!(printed, "moved\n");
    assert!(
        matches!(&ended, Ended::Stopped(said) if said.contains("this `lock` takes the same `shared` value twice")),
        "{ended:?}"
    );
}

/// A deadlock the runtime finds stops the program with exit code 1 after what it printed, as
/// `docs/language.md` says: `main` receiving from a channel nothing sends to, sending to one
/// that's full, waiting at a `with launch` block's end for a task that receives from one, and a
/// `parallel` call's four rounds each receiving from one. What each prints is what the compiled
/// program printed, the same on three runs, and the rounds' on 1 and 4 threads. A listener's wait
/// on a channel to every listener, which the runtime doesn't find yet, is still refused, above.
#[test]
fn a_deadlock_the_runtime_finds_stops_the_program_after_what_it_printed() {
    let stuck = |body: &str| {
        let text = format!(
            "
        from std.concurrent import Channel, Closed, Crashed, Timeout, channel

        def drained(jobs: Channel[int], stdout: Stdout):
            stdout.print('task waits')
            number = jobs.receive() else return
            stdout.print('never', number)

        def stuck(stdout: Stdout) fails Crashed | Closed | Timeout:
            with channel[int](size=1) as jobs:
{body}

        def main(stdout: Stdout):
            stuck(stdout) except why:
                stdout.print('failed', why)
            return
        "
        );
        ran(&text)
    };
    let received = stuck(
        "                stdout.print('before')
                number = jobs.receive() else return
                stdout.print('never', number)",
    );
    assert_eq!(received, ("before\n".to_owned(), Ended::Stopped(DEADLOCK.to_owned())));
    let sent = stuck(
        "                jobs.send(1)?
                stdout.print('sent one')
                jobs.send(2)?
                stdout.print('never')",
    );
    assert_eq!(sent, ("sent one\n".to_owned(), Ended::Stopped(DEADLOCK.to_owned())));
    let ended = stuck(
        "                with launch(() => drained(jobs, stdout)) as task:
                    pass",
    );
    assert_eq!(ended, ("task waits\n".to_owned(), Ended::Stopped(DEADLOCK.to_owned())));
    let rounds = ran("
        from std.concurrent import Channel, channel

        def piece(number: int, between: Channel[int]) -> int:
            return between.receive() else -1

        def main(stdout: Stdout):
            with channel[int](size=1) as between:
                stdout.print([piece(number, between=between) for number in parallel(range(4))])
        ");
    assert_eq!(rounds, (String::new(), Ended::Stopped(DEADLOCK.to_owned())));
}

/// A wait given a timeout times out when nothing else can answer it. What it prints is what the
/// compiled program printed.
#[test]
fn a_wait_times_out_when_nothing_can_answer_it() {
    prints(
        "
        from std.concurrent import Closed, Timeout, channel

        def main(stdout: Stdout) fails Closed:
            with channel[int](size=1) as quiet:
                got = quiet.receive_within(10ms) except Timeout():
                    stdout.print('timed out')
                    return
                stdout.print(got)
            return
        ",
        "timed out\n",
    );
}

/// `main` may take `Env`, `Stderr`, `Signals` and a `Clock`. `Env` is `std.env`'s, run from its
/// own source, with `get` reading the variables the oracle was started with and `secret`
/// failing with `Missing`. Standard error is written and not compared, no signal ever comes, and
/// a `Clock`'s `sleep` moves it on without waiting. A duration written out is `std.time`'s
/// `Duration`. What it prints is what the compiled program printed.
#[test]
fn main_takes_env_stderr_signals_and_a_clock() {
    prints(
        "
        from std.time import Clock, Duration

        def told(env: Env) -> str:
            _ = env.secret('ORACLE_TEST_VARIABLE_NEVER_SET') except missing:
                return f'{missing.name} is not set'
            return 'it is set'

        def main(stdout: Stdout, stderr: Stderr, env: Env, signals: Signals, clock: Clock) fails IoError:
            stdout.print(env.get('ORACLE_TEST_VARIABLE_NEVER_SET') is None, signals.stopping(), told(env))
            stderr.write_line('to standard error')?
            started = clock.now()
            clock.sleep(1500ms)
            stdout.print(not clock.now() - started < 1500ms, 90s, Duration.millis(16), clock.time().year() >= 2026)
            return
        ",
        "True False ORACLE_TEST_VARIABLE_NEVER_SET is not set\nTrue 1min30s 16ms True\n",
    );
}

/// `std.random`'s `Random` runs from its own source, its draws as `docs/language.md` pins them, so
/// a seed gives the compiled program's numbers exactly. An `Entropy`'s bytes and seeds differ
/// every run, so only what holds for any of them is printed. What it prints is what the compiled
/// program printed.
#[test]
fn a_seeded_random_draws_the_compiled_numbers_and_entropy_seeds_one() {
    prints(
        "
        from std.random import Random

        def main(stdout: Stdout, entropy: Entropy):
            mut dice = Random(seed=7)
            rolls = [dice.randint(1, high=6) for _ in range(6)]
            stdout.print(rolls, dice.choice(['fox', 'den', 'kit']), dice.random(), dice.uniform(2.0, high=4.0))
            stdout.print(len(entropy.bytes(16)), entropy.bytes(0) == bytes())
            mut drawn = Random(seed=entropy.seed())
            roll = drawn.randint(1, high=6)
            stdout.print(1 <= roll <= 6)
            return
        ",
        "[3, 1, 6, 4, 3, 2] den 0.3280767391525029 2.2685165976168973\n16 True\nTrue\n",
    );
}

#[test]
fn a_json_load_that_fails_says_what_and_where_as_std_json_does() {
    prints(
        "
        from std import json

        struct Point:
            x: int
            y: int

        def why[T](run: () -> T fails ParseError) -> str:
            _ = run() except problem:
                return f'{problem.message} ({problem.line}:{problem.column})'
            return 'read'

        def main(stdout: Stdout):
            stdout.print(why(() => json.loads[list[Point]]('[\\n  {\\'x\\': 1,}\\n]')?))
            stdout.print(why(() => json.loads[list[Point]]('[{\\'x\\': 1, \\'y\\': \\'2\\'}]')?))
            stdout.print(why(() => json.loads[Point]('{\\'x\\': 1}')?))
            stdout.print(why(() => json.loads[Point]('{\\'x\\': 1, \\'y\\': 2, \\'z\\': 3}')?))
            stdout.print(why(() => json.loads[Point]('{\\'x\\': 1, \\'x\\': 2, \\'y\\': 3}')?))
            stdout.print(why(() => json.loads[dict[int, int]]('{\\'01\\': 1}')?))
        ",
        "expected a key in double quotes, but found `}`, at line 2, column 11 (2:11)\n\
         `[0].y` should be an int, but it's a str (0:0)\n\
         `y` is missing (0:0)\n\
         `Point` has no field \"z\", but the text has one (0:0)\n\
         the text has `x` twice (0:0)\n\
         the text has the key \"01\", which isn't how that int is written (0:0)\n",
    );
}

#[test]
fn a_json_enum_with_a_key_after_its_case_fails_once_its_fields_have_read() {
    prints(
        "
        from std import json

        enum Shape:
            Box(width: int, height: int)
            Dot

        struct Holder:
            shape: Shape

        def why[T](run: () -> T fails ParseError) -> str:
            _ = run() except problem:
                return problem.message
            return 'read'

        def main(stdout: Stdout):
            stdout.print(why(() => json.loads[Shape]('{\\'Box\\': {\\'width\\': 1, \\'height\\': 2}, \\'extra\\': 1}')?))
            stdout.print(why(() => json.loads[Shape]('{\\'Box\\': {\\'width\\': \\'no\\', \\'height\\': 2}, \\'extra\\': 1}')?))
            stdout.print(why(() => json.loads[Shape]('{\\'Dot\\': {}, \\'Box\\': 1}')?))
            stdout.print(why(() => json.loads[Holder]('{\\'shape\\': {\\'Dot\\': {}, \\'a\\\\nb\\': [1]}}')?))
        ",
        "the text has \"extra\" after its case, but an enum is written as an object with its case as the only key\n\
         `width` should be an int, but it's a str\n\
         the text has \"Box\" after its case, but an enum is written as an object with its case as the only key\n\
         `shape` has \"a\\nb\" after its case, but an enum is written as an object with its case as the only key\n",
    );
}

#[test]
fn json_refuses_a_failing_load_it_cant_be_sure_of_and_stops_on_nan() {
    let (printed, ended) = ran("
        from std import json

        struct Note:
            text: str
            @skip
            seen = 0

        def main(stdout: Stdout):
            stdout.print('before')
            _ = json.loads[Note]('{\\'text\\': \\'a\\', \\'\\': 1}') except ParseError():
                Note(text='')
        ");
    assert_eq!(printed, "");
    assert_eq!(
        ended,
        Ended::Unsupported("json.loads failing (a key that names a field `@skip` leaves out)".to_owned())
    );
    let (printed, ended) = ran("
        from std import json, math

        struct Stats:
            values: list[float]

        def main(stdout: Stdout):
            stdout.print('before')
            stdout.print(json.dumps([Stats(values=[1.0]), Stats(values=[2.0, math.NAN])]))
        ");
    assert_eq!(printed, "before\n");
    assert!(
        matches!(&ended, Ended::Stopped(said) if said.contains("`[1].values[1]` is nan, which JSON can't write")),
        "{ended:?}"
    );
}

#[test]
fn a_file_written_is_read_back_and_writing_again_replaces_it() {
    prints_in_work(
        "
        def main(stdout: Stdout, fs: Fs) fails IoError:
            stdout.print(fs.exists('notes.txt')?)
            fs.write_text('notes.txt', 'a longer first note')?
            fs.write_text('notes.txt', 'short')?
            stdout.print(fs.read_text('notes.txt')?, fs.metadata('notes.txt')?.size, fs.exists('notes.txt')?)
            stdout.print(fs.read_text_if_present('none.txt')? else 'nothing yet')
            fs.remove('notes.txt')?
            stdout.print(fs.exists('notes.txt')?)
        ",
        "False\nshort 5 True\nnothing yet\nFalse\n",
    );
}

#[test]
fn a_file_made_in_a_with_block_is_written_in_pieces_and_takes_its_place_when_the_block_ends() {
    prints_in_work(
        "
        error Stop:
            why: str

        def save(fs: Fs, lines: list[str]) -> bool fails IoError | Stop:
            with fs.create('save.txt')? as file:
                for line in lines:
                    if line == '': fail Stop('an empty line')
                    file.write(line)?
                    file.write_line('!')?
                return fs.exists('save.txt')?
            return False

        def main(stdout: Stdout, fs: Fs) fails IoError | Stop:
            stdout.print(save(fs, ['one', 'two'])?)
            stdout.print(fs.read_text('save.txt')?)
            _ = save(fs, ['three', '']) except why:
                stdout.print(why)
                False
            mut found = list[str]()
            with fs.open('save.txt')? as file:
                found.append(file.read_line()? else 'none')
                for line in file.lines()?:
                    found.append(line)
                found.append(str(file.read_line()? is None))
            stdout.print(found)
        ",
        "False\none!\ntwo!\n\nStop(why=\"an empty line\")\n[\"one!\", \"two!\", \"True\"]\n",
    );
}

#[test]
fn a_folder_is_listed_in_order_and_walked_a_folder_before_what_is_in_it() {
    prints_in_work(
        "
        from std.path import Path

        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.make_dir('game/world')?
            fs.write_text('game/world/map.fen', '')?
            fs.write_text('game/main.fen', '')?
            fs.write_text('game/Notes.txt', '')?
            stdout.print([path.text for path in fs.list_dir('game')?])
            stdout.print([path.text for path in fs.walk(Path('game'))?])
            first = fs.list_dir('./game/')?
            stdout.print(first[0], fs.metadata('game/world')?.is_dir)
        ",
        "[\"game/Notes.txt\", \"game/main.fen\", \"game/world\"]\n\
         [\"game/Notes.txt\", \"game/main.fen\", \"game/world\", \"game/world/map.fen\"]\n\
         Path(text=\"./game/Notes.txt\") True\n",
    );
}

/// What makes each of these fail, as `why` shows it, indented as the programs that use it are.
const WHY: &str = "
        def why[T](run: () -> T fails IoError) -> str:
            _ = run() except failed:
                return repr(failed)
            return 'fine'
";

#[test]
fn the_file_system_fails_with_the_case_and_the_words_the_runtime_gives() {
    prints_in_work(
        &format!(
            "{WHY}
        def read(fs: Fs, path: str) -> str fails IoError:
            with fs.open(path)? as file:
                return file.read_text()?
            return ''

        def removed(fs: Fs, path: str) -> str fails IoError:
            fs.remove(path)?
            return 'removed'

        def made(fs: Fs, path: str) -> str fails IoError:
            fs.make_dir(path)?
            return 'made'

        def written(fs: Fs) -> str fails IoError:
            with fs.open('note.txt')? as file:
                file.write_line('more')?
            return 'written'

        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.make_dir('folder/inner')?
            fs.write_text('note.txt', 'x')?
            stdout.print(why(() => fs.read_text('missing.txt')?))
            stdout.print(why(() => fs.read_text('folder')?))
            stdout.print(why(() => read(fs, 'folder')?))
            stdout.print(why(() => fs.list_dir('missing')?))
            stdout.print(why(() => removed(fs, 'folder')?))
            stdout.print(why(() => made(fs, 'note.txt')?))
            stdout.print(why(() => written(fs)?))
            stdout.print(why(() => fs.read_text('a\\x00b')?))
        "
        ),
        "IoError.NotFound(path=\"missing.txt\")\n\
         IoError.Other(reason=\"it's a folder, not a file\")\n\
         IoError.Other(reason=\"it's a folder, not a file\")\n\
         IoError.NotFound(path=\"missing\")\n\
         IoError.Other(reason=\"the folder isn't empty\")\n\
         IoError.AlreadyExists(path=\"note.txt\")\n\
         IoError.Other(reason=\"this file was opened to read, not to write\")\n\
         IoError.Other(reason=\"`a\\\\0b` can't be a path: it holds a NUL\")\n",
    );
}

/// Windows says a path through a file isn't there, so the oracle refuses one there.
#[cfg(unix)]
#[test]
fn a_path_through_a_file_fails_as_not_a_folder() {
    prints_in_work(
        &format!(
            "{WHY}
        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.write_text('note.txt', 'x')?
            stdout.print(why(() => fs.read_text('note.txt/x')?))
            stdout.print(why(() => fs.exists('note.txt/x')?))
            stdout.print(why(() => fs.list_dir('note.txt')?))
        "
        ),
        &"IoError.Other(reason=\"part of the path is a file, not a folder\")\n".repeat(3),
    );
}

#[test]
fn a_narrowed_fs_reaches_only_its_folder_and_a_read_only_one_changes_nothing() {
    prints_in_work(
        "
        def why[T](run: () -> T fails IoError) -> str:
            _ = run() except failed:
                return failed.message()
            return 'fine'

        def wrote(fs: Fs, path: str) -> str fails IoError:
            fs.write_text(path, 'x')?
            return 'written'

        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.make_dir('saves/slot1')?
            fs.write_text('secret.txt', 'hunter2')?
            saves = fs.only('saves')
            saves.write_text('saves/slot1/game.json', '{}')?
            stdout.print(saves.read_text('saves/./slot1/../slot1/game.json')?, saves.exists('saves')?)
            stdout.print(why(() => saves.read_text('secret.txt')?))
            stdout.print(why(() => saves.read_text('saves/../secret.txt')?))
            stdout.print(why(() => saves.only('other').exists('other')?))
            stdout.print(why(() => fs.only('missing').exists('missing/x')?))
            reader = saves.read_only()
            stdout.print(reader.read_text('saves/slot1/game.json')?, why(() => wrote(reader, 'saves/new.txt')?))
            stdout.print(why(() => saves.read_text('saves/a\\x00b')?))
        ",
        "{} True\n\
         `secret.txt`: it's outside what this Fs can reach: `/work/saves`\n\
         `saves/../secret.txt`: it's outside what this Fs can reach: `/work/saves`\n\
         `other`: it's outside what this Fs can reach: nothing\n\
         nothing is at `missing/x`\n\
         {} `saves/new.txt`: this Fs only reads, so it can't change it\n\
         `saves/a\\0b` can't be a path: it holds a NUL\n",
    );
}

#[test]
fn a_database_takes_each_kind_of_value_as_a_parameter_and_gives_rows_back_by_kind() {
    prints_in_work(
        "
        from std import sqlite
        from std.sql import SqlError

        struct Item:
            name: str
            weight: float
            data: bytes
            note: str?

        def main(stdout: Stdout, fs: Fs) fails SqlError:
            with sqlite.open(fs, 'kinds.db')? as db:
                stdout.print(db.execute(t'create table items (name text, weight real, data blob, note text)')?)
                first = 'a \\\"quoted\\\"; drop table items; --'
                second = 'b'
                weight = 0.5
                whole = 2
                data = bytes([0, 255])
                stdout.print(db.execute(t'insert into items values ({first}, {weight}, {data}, null), ({second}, {whole}, {data}, {second})')?)
                stdout.print(db.query[Item](t'select name, weight, data, note from items order by name')?)
                stdout.print(db.execute(t'create table flags (value int)')?)
                stdout.print(db.execute(t'insert into flags values ({True}), ({False}), ({whole})')?)
                stdout.print(db.query[int](t'select value from flags order by value')?, db.query[float](t'select value from flags where value < {whole}')?)
                stdout.print(db.query[bool](t'select value from flags where value < {whole} order by value')?)
                stdout.print(db.query[int?](t'select null union all select {whole}')?, db.query[bytes](t'select data from items limit 1')?)
                stdout.print(db.execute(t'select value from flags')?, db.execute(t'update flags set value = value + {whole} where value > {0}')?)
        ",
        "0\n\
         2\n\
         [Item(name=\"a \\\"quoted\\\"; drop table items; --\", weight=0.5, data=bytes([0, 255]), note=None), Item(name=\"b\", weight=2.0, data=bytes([0, 255]), note=\"b\")]\n\
         2\n\
         3\n\
         [0, 1, 2] [1.0, 0.0]\n\
         [False, True]\n\
         [None, 2] [bytes([0, 255])]\n\
         0 2\n",
    );
}

#[test]
fn a_database_fails_with_the_case_of_sql_error_each_mistake_is() {
    prints_in_work(
        "
        from std import math, sqlite
        from std.sql import SqlError

        struct Pair:
            a: int
            b: str

        def outcome[T](run: () -> T fails SqlError) -> str:
            _ = run() except why:
                case .Constraint(reason=reason):
                    return f'Constraint: {reason}'
                case .Busy(reason=reason):
                    return f'Busy: {reason}'
                case .Column(name=name, reason=reason):
                    return f'Column {name}: {reason}'
                case .Failed(reason=reason):
                    return f'Failed: {reason}'
            return 'fine'

        def main(stdout: Stdout, fs: Fs) fails SqlError:
            with sqlite.open(fs, 'rules.db')? as db:
                _ = db.execute(t'create table rules (id int primary key, name text not null, size int check (size > 0))')?
                one = 1
                name = 'a'
                _ = db.execute(t'insert into rules values ({one}, {name}, {one})')?
                stdout.print(outcome(() => db.execute(t'insert into rules values (2, null, 1)')?))
                stdout.print(outcome(() => db.execute(t'insert into rules values (3, {name}, 0)')?))
                stdout.print(outcome(() => db.execute(t'insert into rules values ({one}, {name}, 1)')?))
                stdout.print(outcome(() => db.execute(t'create tabel nope (x int)')?))
                stdout.print(outcome(() => db.query[int](t'select x from missing')?))
                stdout.print(outcome(() => db.execute(t'delete from rules; delete from rules')?))
                stdout.print(outcome(() => db.query[int](t'select id from rules where id = ?1 or id = {one}')?))
                stdout.print(outcome(() => db.execute(t'insert into rules values (7, {name}, {math.NAN})')?))
                stdout.print(outcome(() => db.query[int](t'select name from rules')?))
                stdout.print(outcome(() => db.query[int](t'select id, name from rules')?))
                stdout.print(outcome(() => db.query[Pair](t'select id as a from rules')?))
                stdout.print(outcome(() => db.query[Pair](t'select id as a, name as b, size from rules')?))
                stdout.print(outcome(() => db.query[bool](t'select size + 1 from rules')?))
                stdout.print(db.query[Pair](t'select id as a, name as b from rules')?)
        ",
        "Constraint: NOT NULL constraint failed: rules.name\n\
         Constraint: CHECK constraint failed: size > 0\n\
         Constraint: UNIQUE constraint failed: rules.id\n\
         Failed: near \"tabel\": syntax error in create tabel nope (x int) at offset 7\n\
         Failed: no such table: missing\n\
         Failed: a query is one statement: give each its own `execute`\n\
         Failed: the query has 1 values but 2 places for them: a `?` or `:name` in its text is one\n\
         Failed: NaN can't be stored: SQLite would turn it into NULL\n\
         Column name: it holds text, not an int\n\
         Column id: a row of 2 columns can't be read as one value: read it into a struct\n\
         Column b: the query gives no column for this field\n\
         Column size: `Pair` has no field called `size`\n\
         Column size + 1: it holds 2, and a bool is 0 or 1\n\
         [Pair(a=1, b=\"a\")]\n",
    );
}

#[test]
fn a_transaction_is_kept_when_its_block_ends_and_undone_when_it_fails() {
    prints_in_work(
        "
        from std import sqlite
        from std.sql import SqlError

        error Broke:
            why: str

        def undone(db: sqlite.Database, stdout: Stdout) fails SqlError | Broke:
            with db.transaction()? as tx:
                stdout.print(tx.execute(t'insert into kept values (10), (20)')?, tx.query[int](t'select count(*) from kept')?)
                fail Broke(why='on purpose')

        def main(stdout: Stdout, fs: Fs) fails SqlError | IoError:
            with sqlite.open(fs, 'kept.db')? as db:
                _ = db.execute(t'create table kept (value int)')?
                with db.transaction()? as tx:
                    stdout.print(tx.execute(t'insert into kept values (1), (2)')?, tx.query[int](t'select count(*) from kept')?)
                    stdout.print(db.query[int](t'select count(*) from kept')?)
                    waited = db.execute(t'insert into kept values (3)') except why:
                        stdout.print(why.message())
                        -1
                    stdout.print(waited)
                stdout.print(db.query[int](t'select count(*) from kept')?)
                undone(db, stdout) except why:
                    stdout.print(why)
                stdout.print(db.query[int](t'select value from kept order by value')?)
            with sqlite.open(fs, 'kept.db')? as again:
                stdout.print(again.query[int](t'select sum(value) from kept')?, fs.exists('kept.db')?)
        ",
        "2 [2]\n\
         [0]\n\
         another writer held the database for over 5 seconds; inside a transaction, query through it rather than the database\n\
         -1\n\
         [2]\n\
         2 [4]\n\
         Broke(why=\"on purpose\")\n\
         [1, 2]\n\
         [3] True\n",
    );
}

#[test]
fn what_a_database_does_past_ordinary_queries_on_its_own_tables_is_refused() {
    let uses = [
        "_ = db.query[str](t'pragma journal_mode')?",
        "_ = db.query[int](t'select random()')?",
        "_ = db.query[float](t'select julianday()')?",
        "_ = db.execute(t'create temp table scratch (x int)')?",
        "_ = db.execute(t'create view seen as select 1')?",
        "_ = fs.read_text('kept.db')?",
        "_ = fs.list_dir('.')?",
        "fs.remove('kept.db')?",
        "fs.write_text('kept.db', 'over')?",
    ];
    for used in uses {
        let (_, ended) = ran_in_work(&format!(
            "
            from std import sqlite
            from std.sql import SqlError

            def main(fs: Fs) fails SqlError | IoError:
                with sqlite.open(fs, 'kept.db')? as db:
                    {used}
            "
        ));
        assert!(matches!(ended, Ended::Unsupported(_)), "{used}: {ended:?}");
    }
    let opened = [
        "fs.write_text('kept.db', 'text')?\n    with sqlite.open(fs, 'kept.db')? as db:",
        "with sqlite.open(fs, 'missing/kept.db')? as db:",
        "fs.make_dir('saves')?\n    with sqlite.open(fs.only('saves'), 'saves/kept.db')? as db:",
        "with sqlite.open(fs.read_only(), 'kept.db')? as db:",
    ];
    for opening in opened {
        let source = format!(
            "from std import sqlite\nfrom std.sql import SqlError\n\ndef main(fs: Fs) fails SqlError | IoError:\n    {opening}\n        _ = db.execute(t'create table t (x int)')?\n"
        );
        let (_, ended) = ran_in_work(&source);
        assert!(matches!(ended, Ended::Unsupported(_)), "{opening}: {ended:?}");
    }
}

#[test]
fn a_path_is_its_text_and_its_methods_only_look_at_it() {
    prints_in_work(
        "
        from std.path import Path

        def main(stdout: Stdout, fs: Fs) fails IoError:
            slot = Path('saves') / 'slot1.json'
            stdout.print(slot, slot.name(), slot.stem(), slot.extension(), slot.parent().text, slot.is_absolute())
            stdout.print(Path('a/./b/../c').normalize().text, Path('/a/../../b').normalize().text, Path('x').join('/etc').text)
            inside = Path('www').inside('/css/site.css') except why:
                Path(why.message)
            outside = Path('www').inside('../secret') except why:
                Path(why.message)
            stdout.print(inside.text, outside.text)
            fs.write_text(slot.name(), 'kept')?
            stdout.print(fs.read_text(Path('slot1.json'))?)
        ",
        "Path(text=\"saves/slot1.json\") slot1.json slot1 json saves False\n\
         a/c /b /etc\n\
         www/css/site.css `../secret` would leave `www`\n\
         kept\n",
    );
}

#[test]
fn a_program_makes_an_io_error_of_its_own() {
    prints_in_work(
        "
        def checked(fs: Fs, path: str) -> str fails IoError:
            text = fs.read_text(path) except why:
                case .NotFound():
                    fail .Other(reason=f'{path} is missing: {why.message()}')
                case _:
                    fail why
            return text

        def main(stdout: Stdout, fs: Fs):
            _ = checked(fs, 'missing.txt') except why:
                stdout.print(repr(why), why.message())
                ''
        ",
        "IoError.Other(reason=\"missing.txt is missing: nothing is at `missing.txt`\") missing.txt is missing: nothing is at `missing.txt`\n",
    );
}

#[test]
fn what_differs_from_one_system_to_another_is_refused() {
    let refused = |body: &str, why: &str| {
        let source = format!("def main(stdout: Stdout, fs: Fs) fails IoError:\n{body}");
        let (printed, ended) = ran_in_work(&source);
        assert_eq!((printed.as_str(), ended), ("", Ended::Unsupported(why.to_owned())), "{body}");
    };
    refused(
        "    with fs.create('made.txt')? as file:\n        stdout.print(fs.list_dir('.')?)\n",
        "what's in a folder while a file is made in it, under a name holding the process's number",
    );
    refused(
        "    fs.write_text('Notes.txt', 'a')?\n    stdout.print(fs.exists('notes.txt')?)\n",
        "the name `notes.txt` beside `Notes.txt`, which some systems take as the same name",
    );
    refused(
        "    fs.write_text('/made-by-the-oracle.txt', 'x')?\n",
        "changing something outside the folder the program runs in",
    );
    refused(
        "    stdout.print(fs.metadata('.')?.is_dir)\n    stdout.print(fs.metadata('.')?)\n",
        "a folder's size, which differs from one system to another",
    );
    refused("    _ = fs.read_bytes('a.bin')?\n", "bytes");
}

/// Outside the folder the program runs in, the disk is only looked at: whether a file or a folder
/// is there. The root is, a path through nothing fails as not found, and reading a file that's
/// there is refused, as it is inside for what was there before.
#[test]
fn outside_its_folder_the_disk_is_only_looked_at() {
    let (printed, ended) = ran_in_work(
        "
        def failure(run: () -> str fails IoError) -> str:
            _ = run() except why:
                return why.message()
            return 'read'

        def main(stdout: Stdout, fs: Fs) fails IoError:
            stdout.print(fs.exists('/')?, fs.exists('/no-such-fennec-folder')?)
            stdout.print(failure(() => fs.read_text('/no-such-fennec-folder/settings.toml')?))
        ",
    );
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert!(printed.starts_with("True False\n"), "{printed}");
    let (_, ended) = ran_in_work(
        "
        def main(stdout: Stdout, fs: Fs) fails IoError:
            stdout.print(fs.read_text('/')?)
        ",
    );
    assert!(matches!(ended, Ended::Unsupported(_) | Ended::Stopped(_)), "{ended:?}");
}

/// In the folder the test runs in, the crate's own, the oracle sees what's there, reads none of
/// it, and writes nothing to the disk.
#[test]
fn the_disk_is_looked_at_and_never_written() {
    let (printed, ended) = ran("
        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.write_text('written-by-the-oracle.txt', 'x')?
            fs.make_dir('made-by-the-oracle/inside')?
            stdout.print(fs.exists('Cargo.toml')?, fs.exists('src')?, fs.exists('no-such-file.txt')?)
            stdout.print(fs.read_text('written-by-the-oracle.txt')?, fs.walk('made-by-the-oracle')?)
        ");
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, "True True False\nx [Path(text=\"made-by-the-oracle/inside\")]\n");
    assert!(!std::path::Path::new("written-by-the-oracle.txt").exists());
    assert!(!std::path::Path::new("made-by-the-oracle").exists());
    let (_, ended) = ran("
        def main(stdout: Stdout, fs: Fs) fails IoError:
            stdout.print(fs.read_text('Cargo.toml')?)
        ");
    assert_eq!(
        ended,
        Ended::Unsupported("reading a file that was there before the program started".to_owned())
    );
}

/// A `shared` value is one cell that every copy of it reaches, changed only inside a `lock`, and a
/// failure leaving `lock ... as ref` undoes the block's changes, while `break` keeps them.
#[test]
fn a_lock_changes_the_one_cell_every_copy_reaches_and_a_failure_undoes_it() {
    prints(
        "
        error Refused:
            code: int

        struct Account:
            items: list[int]
            total = 0

        def add(account: shared Account, by: int) fails Refused:
            lock account as ref held:
                held.total += by
                held.items.append(by)
                if by > 5:
                    fail Refused(code=by)

        def peek(account: shared Account) -> int:
            lock account as held:
                if held.total > 100:
                    return -1
                return held.total

        def main(stdout: Stdout):
            shared account = Account(items=[], total=1)
            other = account
            add(account, 2) except why:
                stdout.print('refused', why.code)
            add(other, 7) except why:
                stdout.print('refused', why.code)
            seen = lock account as held:
                held.total * 10
            stdout.print(seen, peek(other))
            for step in range(3):
                lock other as ref held:
                    held.total += step
                    if step == 1:
                        break
            lock account as held:
                stdout.print(held)
        ",
        "refused 7\n30 3\nAccount(items=[2], total=4)\n",
    );
}

/// `==` on two `shared` values compares what they hold, as decided on 2026-11-14, not which cell
/// they are: two cells holding equal values are equal, a cell is equal to itself and to a copy
/// of itself, and a change through one cell is seen by the next comparison.
#[test]
fn two_shared_values_are_equal_when_they_hold_equal_values() {
    prints(
        "
        struct Account:
            total = 0

        def main(stdout: Stdout):
            shared first = Account(total=1)
            shared second = Account(total=1)
            copy = first
            stdout.print(first == second, first == copy, first == first)
            lock second as ref held:
                held.total += 1
            stdout.print(first == second, first != second)
        ",
        "True True True\nFalse True\n",
    );
}

/// An optional of an optional keeps a `None` inside apart from `None`, nothing found (#2413): a
/// `None` popped, got or removed from a `list[int?]` or a `SortedMap[int, str?]`, and a generic
/// `T?` or an `int??` returned that holds an `int?` that's `None`. `else` takes off one level, as
/// narrowing with `is not None` does, and it prints as what it holds. Held to native code, debug
/// and release.
#[test]
fn an_optional_of_an_optional_keeps_a_none_inside_apart() {
    prints(
        r#"
        from std.collections import SortedMap

        def first_of[T](items: list[T]) -> T?:
            if len(items) == 0:
                return None
            return items[0]

        def held_as_found(given: int?) -> int??:
            return given

        def described(found: int??) -> str:
            match found:
                case None:
                    return "nothing"
                case inner:
                    return f"found {inner}"

        def main(stdout: Stdout):
            none_int: int? = None
            mut numbers: list[int?] = [none_int, 5]
            stdout.print(numbers.get(0) is None, numbers.get(9) is None, numbers.get(0) else 8, numbers.get(9) else 8)
            stdout.print(numbers.remove(none_int) is None, numbers.remove(none_int) is None, len(numbers))
            mut map = SortedMap[int, str?]()
            none_str: str? = None
            map[1] = none_str
            stdout.print(map.get(1) is None, map.get(2) is None, map.get(1) else "missing", map.get(2) else "missing")
            stdout.print(map.remove(1) is None, map.remove(1) is None)
            held: list[int?] = [none_int]
            found = first_of(held)
            if found is not None:
                stdout.print("narrowed", found is None, found)
            stdout.print(f"shown {found}", described(found), described(held_as_found(none_int)), described(held_as_found(3)))
            nothing: list[int?] = []
            stdout.print(described(first_of(nothing)), held_as_found(none_int) is None)
            return
        "#,
        "False True None 8\nFalse True 1\nFalse True None missing\nFalse True\nnarrowed True None\nshown None found None found None found 3\nnothing False\n",
    );
}

/// A `T?` that's `None`, passed to a `T??` parameter or put in a `T??` field, is held there
/// apart from nothing, as native code holds it and as a `return` does: a `T??` never collapses
/// into a `T?` (#2465). A `None` written out is nothing at every level, and a `T??` given to a
/// `T??` is kept as it is. A `T??` local the checker narrows to the `T?` it was given is read as
/// that `T?`, natively too, until the checker keeps its levels.
#[test]
fn a_none_put_in_an_optional_of_an_optional_is_held_apart() {
    prints(
        r"
        struct Holder:
            value: int??

        def shown(value: int??) -> bool:
            return value is None

        def given_back(inner: int?) -> int??:
            return inner

        def probe(inner: int?, stdout: Stdout):
            stdout.print(shown(inner), shown(None), shown(given_back(inner)))
            held = Holder(value=inner)
            stdout.print(held.value is None, (held.value else 1) is None)
            mut box = Holder(value=None)
            stdout.print(box.value is None)
            box.value = inner
            stdout.print(box.value is None)

        def main(stdout: Stdout):
            probe(None, stdout)
            probe(3, stdout)
        ",
        "False True False\nFalse True\nTrue\nFalse\nFalse True False\nFalse False\nTrue\nFalse\n",
    );
}

/// An optional `shared` value that's there is equal to the cell it holds and never to one that's
/// missing, either way round.
#[test]
fn an_optional_shared_value_is_never_equal_to_a_missing_one() {
    prints(
        "
        struct Account:
            total = 0

        def picked(account: shared Account, there: bool) -> (shared Account)?:
            if there:
                return account
            return None

        def main(stdout: Stdout):
            shared account = Account(total=1)
            held = picked(account, there=True)
            missing = picked(account, there=False)
            stdout.print(held == missing, missing == held, held == account, missing == None)
        ",
        "False False True True\n",
    );
}

/// A type's own `def ==` decides `==` on `shared` values even for a cell compared with itself:
/// there's no shortcut for one cell, as native code has none (#597).
#[test]
fn a_shared_value_is_unequal_to_itself_when_its_def_eq_says_so() {
    prints(
        "
        struct Never:
            n = 0

            def ==(self, other: Never) -> bool:
                return False

        def main(stdout: Stdout):
            shared never = Never(n=1)
            copy = never
            stdout.print(never == never, never == copy, never != never)
        ",
        "False False True\n",
    );
}

/// `parallel` gives its results in the list's order, a sum of ints adds up exactly, each `for ref`
/// round changes its own item, the failure passed up is the first round's that fails, by
/// position, and a stream's item fails only once it's read.
#[test]
fn parallel_rounds_give_what_every_order_gives() {
    prints(
        "
        error Odd:
            number: int

        def halved(number: int) -> int fails Odd:
            if number % 2 == 1:
                fail Odd(number)
            return number // 2

        def evens(numbers: list[int]) -> list[int] fails Odd:
            return [halved(number)? for number in parallel(numbers)]

        struct Spot:
            x: int

        def main(stdout: Stdout):
            stdout.print([number * number for number in parallel(range(6)) if number != 2])
            stdout.print(sum(number * 3 for number in parallel([4, -2, 9])))
            mut spots = [Spot(x=n) for n in range(4)]
            for ref spot in parallel(spots):
                spot.x = spot.x * 10 + 1
            stdout.print(spots)
            shared seen = Spot(x=0)
            for number in parallel(range(5)):
                if number == 3:
                    lock seen as ref held:
                        held.x = number
            lock seen as held:
                stdout.print(held.x)
            found = evens([2, 4, 7, 8, 9]) except why:
                stdout.print('odd', why.number)
                list[int]()
            stdout.print(found)
            stream = (100 // (number - 5) for number in parallel(range(12)))
            mut read = 0
            for value in stream:
                stdout.print('streamed', value)
                read += 1
                if read == 3:
                    break
        ",
        "[0, 1, 9, 16, 25]\n33\n[Spot(x=1), Spot(x=11), Spot(x=21), Spot(x=31)]\n3\nodd 7\n[]\n\
         streamed -20\nstreamed -25\nstreamed -34\n",
    );
}

/// A `for` over `parallel` whose body has a `return` in it runs one round after another, as a
/// plain loop over the list: it prints in order, and the `return`, written as a statement or in an
/// `except` block, leaves the function at the round it's in, with no round after it run. What's
/// printed is what the compiled program printed.
#[test]
fn a_parallel_loop_with_a_return_runs_as_a_plain_loop() {
    prints(
        "
        error Odd:
            number: int

        def halved(number: int) -> int fails Odd:
            if number % 2 == 1:
                fail Odd(number)
            return number // 2

        def first_big(items: list[int], stdout: Stdout) -> int:
            for item in parallel(items):
                stdout.print('looking', item)
                if item > 10:
                    return item
            return -1

        def first_odd(items: list[int], stdout: Stdout) -> int:
            for item in parallel(items):
                half = halved(item) except why:
                    stdout.print('odd', why.number)
                    return item
                stdout.print('half', half)
            return 0

        def main(stdout: Stdout):
            stdout.print(first_big([1, 5, 20, 3, 40], stdout))
            stdout.print(first_big([1, 2], stdout))
            stdout.print(first_odd([4, 8, 3, 6], stdout))
        ",
        "looking 1\nlooking 5\nlooking 20\n20\nlooking 1\nlooking 2\n-1\nhalf 2\nhalf 4\nodd 3\n3\n",
    );
}

/// A `for ref` over `parallel` whose round reads the list it goes over runs as a plain loop, as
/// the checker writes it in to go by position (#2356): each round sees the items as they are now,
/// its own change included, prints in order, and a `return`, written as a statement, in an
/// `except` block or in a loop inside the round, keeps the change the round made. A loop that
/// reads another field of what holds its list still runs as rounds, so printing in it is
/// refused. What's printed is what the compiled program printed.
#[test]
fn a_parallel_for_ref_that_reads_its_list_runs_as_a_plain_loop() {
    prints(
        "
        error Odd:
            number: int

        struct Spot:
            x: int

        def halved(number: int) -> int fails Odd:
            if number % 2 == 1:
                fail Odd(number)
            return number // 2

        def returned(spots: list[Spot]) -> list[Spot]:
            mut kept = spots
            for ref spot in parallel(kept):
                spot.x += 1
                if spot.x > 2:
                    return kept
            return kept

        def returned_from_except(spots: list[Spot]) -> list[Spot]:
            mut kept = spots
            for ref spot in parallel(kept):
                spot.x += 1
                _ = halved(spot.x) except why:
                    spot.x += why.number * 100
                    return kept
            return kept

        def returned_from_inner_loop(spots: list[Spot]) -> list[Spot]:
            mut kept = spots
            for ref spot in parallel(kept):
                spot.x += 1
                for step in range(2):
                    if spot.x + step > 3:
                        return kept
            return kept

        def read_in_round(spots: list[Spot], stdout: Stdout) -> list[Spot]:
            mut kept = spots
            for ref spot in parallel(kept):
                spot.x = spot.x * 10 + 1
                stdout.print('first is', kept[0].x, 'last is', kept[len(kept) - 1].x)
            return kept

        def main(stdout: Stdout):
            stdout.print(returned([Spot(x=0), Spot(x=2), Spot(x=5)]))
            stdout.print(returned_from_except([Spot(x=1), Spot(x=4), Spot(x=6)]))
            stdout.print(returned_from_inner_loop([Spot(x=0), Spot(x=1), Spot(x=7)]))
            stdout.print(read_in_round([Spot(x=1), Spot(x=2), Spot(x=3)], stdout))
        ",
        "[Spot(x=1), Spot(x=3), Spot(x=5)]\n[Spot(x=2), Spot(x=505), Spot(x=6)]\n[Spot(x=1), Spot(x=2), Spot(x=8)]\n\
         first is 11 last is 3\nfirst is 11 last is 3\nfirst is 11 last is 31\n[Spot(x=11), Spot(x=21), Spot(x=31)]\n",
    );
    let (_, ended) = ran("
        struct Spot:
            x: int

        struct World:
            spots: list[Spot]
            score: int

        def main(stdout: Stdout):
            mut world = World(spots=[Spot(x=1), Spot(x=2)], score=7)
            for ref spot in parallel(world.spots):
                spot.x += world.score
            stdout.print(world.spots)
            for ref spot in parallel(world.spots):
                stdout.print(spot.x)
        ");
    assert_eq!(
        ended,
        Ended::Unsupported("printing inside a `parallel` round, whose order isn't promised".to_owned())
    );
}

/// A set or dict comprehension over `parallel` takes its rounds' items in the list's order, as a
/// plain one does: the first of equal items is kept where it came, and a key keeps its first place
/// and its last value. What's printed is what the compiled program printed.
#[test]
fn a_set_or_dict_made_over_parallel_holds_its_items_in_the_lists_order() {
    prints(
        "
        def main(stdout: Stdout):
            stdout.print({item % 5 for item in parallel([7, 3, 12, 8, 2, 7])})
            stdout.print({item for item in parallel(list(range(3000))) if item % 1000 == 7})
            stdout.print({(left, right % 2) for left, right in parallel([(1, 4), (2, 5), (1, 6)])})
            stdout.print(len({item // 3 for item in parallel(list(range(5000)))}))
            stdout.print({item % 5: item for item in parallel([7, 3, 12, 8, 2, 7])})
            stdout.print({item: item * 2 for item in parallel(list(range(3000))) if item % 1000 == 7})
            stdout.print({left: right for left, right in parallel([(1, 4), (2, 5), (1, 6)])})
            stdout.print(len({item // 3: item for item in parallel(list(range(5000)))}))
        ",
        "{2, 3}\n{7, 1007, 2007}\n{(1, 0), (2, 1)}\n1667\n{2: 7, 3: 8}\n{7: 14, 1007: 2014, 2007: 4014}\n\
         {1: 6, 2: 5}\n1667\n",
    );
}

/// What `parallel` doesn't promise an order for is refused: printing in a round, a `shared` value
/// one round changes and another reaches, and a round that may not run changing something.
#[test]
fn what_parallel_rounds_could_do_in_another_order_is_refused() {
    let refused = |body: &str, why: &str| {
        let (_, ended) = ran(&format!(
            "error Odd:\n    number: int\n\nstruct Total:\n    count: int\n\n\
             def odd(number: int) -> int fails Odd:\n    if number % 2 == 1:\n        fail Odd(number)\n    \
             return number\n\ndef main(stdout: Stdout) fails Odd:\n    shared total = Total(count=0)\n{body}"
        ));
        assert_eq!(ended, Ended::Unsupported(why.to_owned()), "{body}");
    };
    refused(
        "    for number in parallel(range(3)):\n        stdout.print(number)\n",
        "printing inside a `parallel` round, whose order isn't promised",
    );
    refused(
        "    for number in parallel(range(3)):\n        lock total as ref held:\n            held.count += number\n",
        "a `shared` value one `parallel` round changes and another reaches, in an order that isn't promised",
    );
    refused(
        "    for number in parallel([1, 2, 4]):\n        _ = odd(number)?\n        if number == 4:\n            \
         lock total as ref held:\n                held.count += number\n",
        "a `parallel` round that may not run changing a `shared` value",
    );
}

/// A `sum` over `parallel(xs)` that compiled code lifts into ranges, of one name with no `if` and
/// no `?`, adds in the runtime's fixed tree: ranges of at most 1,024 positions, each from its
/// first part on, halved at the middle, the earlier half first, and the start added last. So a
/// float sum gives the tree's bits, not the eight running totals a plain `sum` keeps, and an int
/// sum overflows where the tree does, not where adding in order would. Any other form adds in
/// order, as a plain `sum` does. What's printed is what the compiled program printed.
#[test]
fn a_sum_over_parallel_adds_in_the_runtimes_tree_where_its_lifted_and_in_order_where_not() {
    prints(
        "
        def main(stdout: Stdout):
            floats = [float(at % 7) * 0.1 + (1.0e16 if at % 1000 == 3 else 0.0) - (1.0e16 if at % 1000 == 501 else 0.0) for at in range(3000)]
            stdout.print(sum(x for x in parallel(floats)), sum(x for x in floats))
            stdout.print(sum((x for x in parallel(floats)), start=0.5))
            stdout.print(sum(x for x in parallel(floats) if True))
            stdout.print(sum(x for x in parallel([-0.0])), sum(x for x in parallel(list[float]())))
            ints = list(range(5000))
            stdout.print(sum(x * x for x in parallel(ints)), sum(x for x in parallel(ints) if x % 3 == 0))
            stdout.print(sum((x for x in parallel(ints)), start=100))
            pairs = [(at, at * 2) for at in range(2000)]
            stdout.print(sum(left * right for left, right in parallel(pairs)))
            stdout.print(sum((x for x in parallel(list[int]())), start=4))
            big = 9223372036854775807 // 1024
            stdout.print(sum(x for x in parallel([big if at <= 1024 else -big for at in range(2048)])))
        ",
        "300.0 688.0\n300.5\n688.0\n0.0 0.0\n41654167500 4165833\n12497600\n5329334000\n4\n18014398509481982\n",
    );
    let stopped = |body: &str, said: &str| {
        let (printed, ended) = ran(&format!(
            "def picked(at: int, x: int) -> int:\n    if at == 1500 or at == 3000:\n        return [7][at]\n    \
             return x\n\ndef main(stdout: Stdout):\n    big = 9223372036854775807 // 1024\n{body}"
        ));
        assert_eq!(printed, "", "{body}");
        assert!(matches!(&ended, Ended::Stopped(stopped) if stopped.contains(said)), "{body}: {ended:?}");
    };
    // Adding in order overflows at the 1,025th part, where the tree adds the two leaves' totals.
    stopped(
        "    stdout.print(sum(x for x in parallel([big if at <= 1024 else -big for at in range(2048)]) if True))\n",
        "this calculation overflows an int",
    );
    // The crash at 1500 is in the range from 1024, below the range from 2048 whose halves' total
    // overflows, so it's the one passed up.
    stopped(
        "    stdout.print(sum(picked(at, x=big) for at in parallel([at for at in range(4096) if at != 3000])))\n",
        "index 1500 is out of range for length 1",
    );
    // The range from 0 overflows adding its halves, below the crash at 3000.
    stopped(
        "    stdout.print(sum(picked(at, x=big) for at in parallel([at for at in range(4096) if at != 1500])))\n",
        "this calculation overflows an int",
    );
}

/// A `with launch` block runs first, then its task, reading what the block sent in order. The
/// `with` ends with the task's failure, or `Crashed` for a crash, where a crash in a `lock` undoes
/// its changes. Printing in both the block and the task is refused.
#[test]
fn a_launched_task_reads_what_its_block_sent_and_its_failure_is_the_withs() {
    prints(
        "
        from std.concurrent import Closed, Crashed, Inbox, Timeout

        error Refused:
            code: int

        struct Point:
            x: int
            y: int

        def tallying(inbox: Inbox[int], totals: shared Point):
            for number in inbox:
                lock totals as ref held:
                    held.y += 1
                    held.x += 100 // number

        def tally(totals: shared Point, items: list[int]) fails Crashed | Closed | Timeout:
            with launch((inbox) => tallying(inbox, totals))? as sends:
                for item in items:
                    sends.send(item)?

        def guarding(inbox: Inbox[int], stdout: Stdout) fails Refused:
            for number in inbox:
                stdout.print('task', number)
                if number == 7:
                    fail Refused(code=number)

        def guard_all(items: list[int], stdout: Stdout) fails Crashed | Closed | Timeout | Refused:
            with launch((inbox) => guarding(inbox, stdout)?)? as sends:
                for item in items:
                    sends.send(item)?

        def main(stdout: Stdout):
            shared totals = Point(x=0, y=0)
            tally(totals, [1, 2, 5]) except why:
                case Crashed(why=reason):
                    stdout.print('crashed', reason)
                case Closed() | Timeout():
                    stdout.print('other')
            tally(totals, [4, 2, 0]) except why:
                case Crashed(why=reason):
                    stdout.print('crashed', reason)
                case Closed() | Timeout():
                    stdout.print('other')
            lock totals as held:
                stdout.print(held)
            guard_all([1, 3, 7], stdout) except why:
                case Refused(code):
                    stdout.print('refused', code)
                case Crashed() | Closed() | Timeout():
                    stdout.print('other')
        ",
        "crashed the task crashed\nPoint(x=245, y=5)\ntask 1\ntask 3\ntask 7\nrefused 7\n",
    );
    // The block prints before it sends, and the task prints after it receives, so the order is
    // promised.
    prints(
        "
        from std.concurrent import Closed, Crashed, Inbox, Timeout

        def echoing(inbox: Inbox[int], stdout: Stdout):
            for number in inbox:
                stdout.print('task', number)

        def main(stdout: Stdout) fails Crashed | Closed | Timeout:
            with launch((inbox) => echoing(inbox, stdout))? as sends:
                stdout.print('block')
                sends.send(1)?
        ",
        "block\ntask 1\n",
    );
    // Here nothing orders the task's first print and the block's.
    let (_, ended) = ran("
        from std.concurrent import Closed, Crashed, Inbox, Timeout

        def echoing(inbox: Inbox[int], stdout: Stdout):
            stdout.print('task')
            for number in inbox:
                stdout.print('task', number)

        def main(stdout: Stdout) fails Crashed | Closed | Timeout:
            with launch((inbox) => echoing(inbox, stdout))? as sends:
                stdout.print('block')
                sends.send(1)?
        ");
    assert_eq!(
        ended,
        Ended::Unsupported("printing in two tasks at once, whose order hangs on time".to_owned())
    );
}
