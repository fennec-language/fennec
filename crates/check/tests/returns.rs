//! Every `def` that gives back a value writes its return type, and a `def` with no arrow
//! gives back nothing. Leaving the type out is a mistake whose fix writes it in, and
//! `-> None` is one whose fix removes it. Arrow functions never write one.

use fennec_check::Type;

use crate::{dedent, helps, problems, problems_as_written, returns};

/// A program with the fixes of its problems made, the way `fennec fix` makes them: round after
/// round, as each fix can show the next, like the `return` a return type written in asks for.
fn fixed(text: &str) -> String {
    let mut source = dedent(text);
    for _ in 0..4 {
        let Ok(module) = fennec_syntax::parse(&source) else { return source };
        let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
        match fennec_syntax::apply_fixes(&source, 0, &diagnostics) {
            Some(changed) if changed != source => source = changed,
            _ => return source,
        }
    }
    source
}

#[test]
fn a_def_that_gives_back_a_value_writes_its_type() {
    let program = "
        struct Enemy:
            health = 100

            def hurt(self, amount: int):
                Enemy(health=self.health - amount)

            def +(self, other: Enemy):
                Enemy(health=self.health + other.health)

        def total(scores: list[int]):
            def double(n: int):
                n * 2
            sum(double(score) for score in scores)

        def main(stdout: Stdout):
            stdout.print(total([1, 2]), Enemy().hurt(5))
    ";
    assert_eq!(
        problems_as_written(program),
        [
            "`hurt` gives back an Enemy, so its return type has to be written @ def hurt(self, amount: int)",
            "`+` gives back an Enemy, so its return type has to be written @ def +(self, other: Enemy)",
            "`total` gives back an int, so its return type has to be written @ def total(scores: list[int])",
            "`double` gives back an int, so its return type has to be written @ def double(n: int)",
        ]
    );
    assert_eq!(
        fixed(program),
        dedent(
            "
            struct Enemy:
                health = 100

                def hurt(self, amount: int) -> Enemy:
                    return Enemy(health=self.health - amount)

                def +(self, other: Enemy) -> Enemy:
                    return Enemy(health=self.health + other.health)

            def total(scores: list[int]) -> int:
                def double(n: int) -> int:
                    return n * 2
                return sum(double(score) for score in scores)

            def main(stdout: Stdout):
                stdout.print(total([1, 2]), Enemy().hurt(5))
            "
        )
    );
}

#[test]
fn the_help_says_what_to_write() {
    let program = "def pair(n: int):\n    (n, 'a')\n";
    assert_eq!(
        crate::check_as_written(&dedent(program)).err().unwrap_or_default(),
        ["`pair` gives back a (int, str), so its return type has to be written @ def pair(n: int)"]
    );
    let source = dedent(program);
    let module = fennec_syntax::parse(&source).unwrap();
    let help: Vec<String> = fennec_check::check(&module)
        .err()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|diagnostic| diagnostic.help)
        .collect();
    assert_eq!(help, ["write it after the parameters: `def pair(n: int) -> (int, str):`"]);
}

#[test]
fn a_generator_gives_back_an_iterator() {
    let program = "
        def countdown(n: int):
            mut at = n
            while at > 0:
                yield at
                at -= 1
    ";
    assert_eq!(
        problems_as_written(program),
        ["`countdown` yields an int, so its return type has to be written @ def countdown(n: int)"]
    );
    assert!(fixed(program).starts_with("def countdown(n: int) -> Iterator[int]:"));
    let program = "def letters() -> Iterator[str]:\n    yield 'a'\n    yield 1\n";
    assert_eq!(
        problems_as_written(program),
        ["what `letters` yields should be a str, but this is an int @ 1"]
    );
    assert_eq!(
        returns("def letters() -> Iterator[str]:\n    yield 'a'\n", "letters"),
        Ok(Type::Iterator(Box::new(Type::Str)))
    );
}

#[test]
fn an_arrow_to_none_is_removed() {
    let program = "
        def greet(stdout: Stdout) -> None:
            stdout.print('hi')

        def wave(stdout: Stdout) -> nothing:
            return stdout.print('o/')
    ";
    assert_eq!(
        problems_as_written(program),
        [
            "`greet` gives back nothing, so it has no arrow @ -> None",
            "`wave` gives back nothing, so it has no arrow @ -> nothing",
        ]
    );
    let fixed = fixed(program);
    assert!(
        fixed.contains("def greet(stdout: Stdout) :") && fixed.contains("def wave(stdout: Stdout) :"),
        "{fixed}"
    );
    assert_eq!(
        helps("def greet(stdout: Stdout) -> None:\n    stdout.print('hi')\n"),
        ["a `def` with no `->` gives back nothing: remove the arrow and what follows it"]
    );
    assert_eq!(
        returns("def f(n: int) -> None:\n    pass\n", "f").err().map(|problems| problems.len()),
        Some(1)
    );
}

#[test]
fn a_def_with_no_arrow_gives_back_nothing() {
    let program = "
        def log(stdout: Stdout, n: int):
            if n > 0:
                stdout.print(n)
                log(stdout, n - 1)

        def main(stdout: Stdout):
            log(stdout, 3)
            stdout.print(log(stdout, 1) + 1)
    ";
    assert_eq!(problems_as_written(program), ["can't use `+` on nothing and an int @ log(stdout, 1) + 1"]);
}

#[test]
fn arrow_functions_never_write_a_return_type() {
    let program = "
        def main(stdout: Stdout):
            stdout.print(sorted(['bb', 'a'], key=name => len(name)))
    ";
    assert_eq!(problems_as_written(program), Vec::<String>::new());
}

#[test]
fn a_type_the_program_cant_name_gets_no_fix() {
    let program = "
        from std.collections import Deque

        def queue():
            Deque.of([1])

        def pool():
            items = [1]
            items.index(1)
    ";
    let source = dedent(program);
    let module = fennec_syntax::parse(&source).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    let fixes: Vec<usize> = diagnostics.iter().map(|diagnostic| diagnostic.fixes.len()).collect();
    assert_eq!(fixes, [1, 1]);
    let program = "
        from std.collections import Table

        def pool():
            mut ids = Table[int]()
            id = ids.add(1)
            id
    ";
    let source = dedent(program);
    let module = fennec_syntax::parse(&source).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics.iter().all(|diagnostic| diagnostic.fixes.is_empty()), "{diagnostics:?}");
}

/// `len(x)` is a builtin, as in Python: it calls the value's own `len`, a built-in value's or
/// a type's `def len(self) -> int`, and gives an int.
#[test]
fn len_calls_the_values_own_len() {
    assert_eq!(
        problems(
            "
            struct Bag:
                items: list[int]

                def len(self) -> int:
                    return len(self.items)

            def main(stdout: Stdout):
                xs = [1, 2]
                bag = Bag(items=xs)
                stdout.print(len(xs) + len(xs + [3]), f\"{len({1: 2})}\", len(str(len('ab'))), len(bag) + 1)
            "
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        problems_as_written(
            "def main(stdout: Stdout):\n    stdout.print(len('a') + 'b', len(1), len([1], [2]))"
        ),
        [
            "can't use `+` on an int and a str @ len(\"a\") + \"b\"",
            "an int has no method `len` @ len(1)",
            "`len` takes one value: `len(x)` @ len([1], [2])",
        ]
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(sorted(['a'], key=len))"),
        ["`len` is called on a value: write `len(x)`, or `x => len(x)` to pass it"]
    );
}

/// A top-level function, type or constant can't take a builtin's name, as a local can't: a
/// `def len` would hide the language's `len` everywhere it's seen, a dependency's code included.
/// A method or a field may, as `len(x)` calls `x.len()`.
#[test]
fn a_top_level_name_cant_be_a_builtins() {
    assert_eq!(
        problems_as_written(
            "def len(text: str) -> int:\n    return 99\n\nstruct range:\n    low: int\n\nenum min:\n    One\n\n\
             error max:\n    why: str\n\ninterface str:\n    def shown(self) -> int\n\ntype list = int\n\n\
             def main(stdout: Stdout):\n    stdout.print(1)\n"
        ),
        [
            "`len` is a builtin's name, which a function can't take @ def len(text: str) -> int",
            "`range` is a builtin's name, which a type can't take @ struct range",
            "`min` is a builtin's name, which a type can't take @ enum min",
            "`max` is a builtin's name, which a type can't take @ error max",
            "`str` is a builtin's name, which an interface can't take @ interface str:\n    def shown(self) -> int\n\n",
            "`list` is a builtin's name, which a type can't take @ type list = int",
        ]
    );
    assert_eq!(
        problems_as_written(
            "struct Box:\n    len: int\n\n    def sum(self) -> int:\n        return self.len\n\n\
             def main(stdout: Stdout):\n    stdout.print(Box(len=2).sum())\n"
        ),
        Vec::<String>::new()
    );
}

/// `x.len()` and `x.abs()` on a value, the forms before 2026-11-11, are refused with what to
/// write: `len` and `abs` are builtins. A type's own `def len` is still how `len(x)` reaches it.
#[test]
fn a_len_or_abs_method_on_a_value_is_refused() {
    assert_eq!(
        problems_as_written(
            "struct Bag:\n    items: list[int]\n\n    def len(self) -> int:\n        return len(self.items)\n\n\
             def main(stdout: Stdout):\n    bag = Bag(items=[1])\n    below = -2\n    stdout.print([1, 2].len(), bag.len(), below.abs(), len(bag))\n"
        ),
        [
            "`len` is a builtin, written `len(x)`, as in Python @ [1, 2].len()",
            "`len` is a builtin, written `len(x)`, as in Python @ bag.len()",
            "`abs` is a builtin, written `abs(x)`, as in Python @ below.abs()",
        ]
    );
}

/// `abs(x)` is a builtin the same way: it gives back a number of the type it's given.
#[test]
fn abs_calls_the_values_own_abs() {
    assert_eq!(
        problems(
            "
            def main(stdout: Stdout):
                x = -2
                stdout.print(abs(x), abs(-3), abs(x * 2 if x > 0 else 1), abs(abs(x)), abs(-1.5) + 0.5)
            "
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        problems_as_written("def main(stdout: Stdout):\n    stdout.print(abs(1) + 'a', abs(1.5) + 1)"),
        [
            "can't use `+` on an int and a str @ abs(1) + \"a\"",
            "can't mix int and float in `+` @ abs(1.5) + 1"
        ]
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(sorted([1], key=abs))"),
        ["`abs` is called on a value: write `abs(x)`, or `x => abs(x)` to pass it"]
    );
}

#[test]
fn a_function_that_says_it_gives_back_a_value_cant_run_off_its_end() {
    let looped = "
        def all(n: int) -> list[int]:
            mut found = []
            for at in range(n):
                found.append(at)
    ";
    let problems = problems_as_written(looped);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("but its last line gives back nothing"), "{problems:?}");

    let failing = "
        error TooBig:
            limit: int

        def word(n: int) -> str fails TooBig:
            if n > 2: fail TooBig(limit=2)
            return \"\"

        def all(n: int) -> list[str] fails TooBig:
            mut found = []
            for at in range(n):
                found.append(word(at)?)
    ";
    assert_eq!(problems_as_written(failing).len(), 1, "a function that can fail too");
}

#[test]
fn leaving_on_every_path_is_giving_something_back() {
    let fine = "
        def sign(n: int) -> int:
            if n < 0:
                return -1
            else:
                return 1

        def forever(n: int) -> int:
            while True:
                if n > 0: return n
    ";
    assert_eq!(problems_as_written(fine), Vec::<String>::new());
}

#[test]
fn a_last_line_after_leaving_on_every_path_still_takes_the_return_type() {
    let fine = "
        def load(n: int) -> list[str]:
            if n > 0:
                return [\"a\"]
            else:
                return [\"b\"]
            return []
    ";
    assert_eq!(problems_as_written(fine), Vec::<String>::new());
}

#[test]
fn a_bare_return_cant_stand_for_a_value() {
    let problems = problems_as_written(
        "
        def f(n: int) -> int:
            if n > 0:
                return
            return n
    ",
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("but this gives back nothing"), "{problems:?}");
    assert_eq!(problems_as_written("def g(n: int):\n    if n > 0:\n        return\n"), Vec::<String>::new());
}

#[test]
fn a_call_that_changes_something_at_the_end_of_a_def_with_no_arrow_is_a_statement() {
    let text = "struct Board:\n    scores: list[int]\n\n    def add(ref self, score: int) -> int:\n        \
        self.scores.append(score)\n        return len(self.scores)\n\n\
        def bump(ref items: list[int]) -> int:\n    items.append(1)\n    return len(items)\n\n\
        def record(ref board: Board):\n    board.add(1)\n\n\
        def grow(ref items: list[int]):\n    bump(ref items)\n";
    let empty: [&str; 0] = [];
    assert_eq!(problems_as_written(text), empty);
}

#[test]
fn a_value_at_the_end_of_a_def_with_no_arrow_still_needs_its_type() {
    let text = "def size(items: list[int]):\n    len(items)\n\ndef main(stdout: Stdout):\n    stdout.print(size([1]))\n";
    assert_eq!(
        problems_as_written(text),
        ["`size` gives back an int, so its return type has to be written @ def size(items: list[int])"]
    );
}

#[test]
fn a_last_match_whose_cases_all_leave_gives_back_what_they_do() {
    let source = "enum Order:\n    Hold\n    Move\n\ndef pick(n: int) -> Order:\n    match n:\n        case 0:\n            return .Hold\n        case _:\n            return .Move\n";
    assert_eq!(problems_as_written(source), Vec::<String>::new());
    let wrong = "def pick(n: int) -> str:\n    match n:\n        case 0:\n            return 1\n        case _:\n            return \"a\"\n";
    assert_eq!(problems_as_written(wrong).len(), 1, "{:?}", problems_as_written(wrong));
    let falls = "def pick(n: int) -> str:\n    match n:\n        case 0:\n            return \"a\"\n        case _:\n            return n + 1\n";
    assert_eq!(problems_as_written(falls).len(), 1, "{:?}", problems_as_written(falls));
}

/// A case that ends in a `with` block whose body returns leaves too, as a plain `return` does,
/// so a last `match` of such cases gives back what they do. A `with` whose body can get past
/// its end doesn't.
#[test]
fn a_last_match_whose_cases_end_in_a_with_that_returns_gives_back_what_they_do() {
    let source = "struct Session:\n    log: str\n\nstruct Lite:\n    name: str\n\n    def session(self) -> with Session:\n        lend Session(log=\"lite\") except:\n            pass\n\n\
        def pick(store: Lite, n: int) -> int:\n    match n:\n        case 0:\n            with store.session() as session:\n                return len(session.log)\n        case _:\n            with store.session() as session:\n                if n > 1:\n                    return 2\n                else:\n                    return 3\n";
    assert_eq!(problems_as_written(source), Vec::<String>::new());
    let falls = source.replace("                    return 3\n", "                    pass\n");
    assert_eq!(problems_as_written(&falls).len(), 1, "{:?}", problems_as_written(&falls));
}
