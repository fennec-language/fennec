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

#[test]
fn an_else_ends_a_chain_at_a_missing_or_failed_step() {
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
            stdout.print(find(1).name else '?', find(2).name else '?')
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
    assert!(said.starts_with("error: this Id refers to something that's been removed\n"), "{said}");
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
         1.0 32.0\n\
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

            def table[T: Serializable](ref self, kind: str, tag: int, ids: list[Id[T]], items: list[T]):
                self.sequence(items)

            def exact(self) -> bool:
                return False

            def slots[T: Serializable](ref self, kind: str, tag: int, slots: list[int], items: list[T]):
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
        "block gave -1\nCrashed(why=\"the task crashed\")\ndone\n",
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
        "second send closed\nblock ends\nCrashed(why=\"the task crashed\")\ndone\n",
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
/// hanging on time; one that waits for what no round sends, which hangs; and the round that
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
    assert_eq!(ended, Ended::Unsupported("every task waits on another, so the program hangs".to_owned()));
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
/// has ended, waits forever, as its block does for it: a hang, which is refused.
#[test]
fn tasks_that_all_wait_are_refused_as_a_hang() {
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
    assert_eq!(ended, Ended::Unsupported("every task waits on another, so the program hangs".to_owned()));
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
                return f'{failed}'
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
        "IoError.NotFound(reason=\"nothing is there\")\n\
         IoError.Other(reason=\"it's a folder, not a file\")\n\
         IoError.Other(reason=\"it's a folder, not a file\")\n\
         IoError.NotFound(reason=\"nothing is there\")\n\
         IoError.Other(reason=\"the folder isn't empty\")\n\
         IoError.AlreadyExists(reason=\"something is there already\")\n\
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
         `secret.txt` is outside what this Fs can reach: `/work/saves`\n\
         `saves/../secret.txt` is outside what this Fs can reach: `/work/saves`\n\
         `other` is outside what this Fs can reach: nothing\n\
         nothing is there\n\
         {} this Fs only reads, so it can't change `saves/new.txt`\n\
         `saves/a\\0b` can't be a path: it holds a NUL\n",
    );
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
                stdout.print(why, why.message())
                ''
        ",
        "IoError.Other(reason=\"missing.txt is missing: nothing is there\") missing.txt is missing: nothing is there\n",
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

/// What `parallel` doesn't promise an order for is refused: printing in a round, a `shared` value
/// one round changes and another reaches, a round that may not run changing something, and a sum
/// added in a tree of the runtime's own, of floats or of ints that could overflow.
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
    refused(
        "    stdout.print(sum(number / 2 for number in parallel(range(3))))\n",
        "a float `sum` over `parallel`, which adds in a tree of its own",
    );
    refused(
        "    stdout.print(sum(number for number in parallel([9223372036854775807, 1, -1])))\n",
        "an int `sum` over `parallel` that may overflow, by how it's split",
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
