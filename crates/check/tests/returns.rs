//! Every `def` that gives back a value writes its return type, and a `def` with no arrow
//! gives back nothing. Leaving the type out is a mistake whose fix writes it in, and
//! `-> None` is one whose fix removes it. Arrow functions never write one.

use fennec_check::Type;

use crate::{dedent, helps, problems_as_written, returns};

/// A program with the fixes of its problems made, the way `fennec fix` makes them.
fn fixed(text: &str) -> String {
    let source = dedent(text);
    let Ok(module) = fennec_syntax::parse(&source) else { return source };
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    fennec_syntax::apply_fixes(&source, 0, &diagnostics).unwrap_or(source)
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
                    Enemy(health=self.health - amount)

                def +(self, other: Enemy) -> Enemy:
                    Enemy(health=self.health + other.health)

            def total(scores: list[int]) -> int:
                def double(n: int) -> int:
                    n * 2
                sum(double(score) for score in scores)

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
        Ok(crate::list(Type::Str))
    );
}

#[test]
fn an_arrow_to_none_is_removed() {
    let program = "
        def greet(stdout: Stdout) -> None:
            stdout.print('hi')

        def wave(stdout: Stdout) -> nothing:
            stdout.print('o/')
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
            stdout.print(sorted(['bb', 'a'], key=name => name.len()))
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
        from std.game import IdMap

        def pool():
            mut ids = IdMap[int]()
            ids.insert(1)
    ";
    let source = dedent(program);
    let module = fennec_syntax::parse(&source).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics.iter().all(|diagnostic| diagnostic.fixes.is_empty()), "{diagnostics:?}");
}

#[test]
fn len_is_a_method_and_its_fix_moves_it_there() {
    let program = "
        def main(stdout: Stdout):
            xs = [1, 2]
            stdout.print(len(xs), len(xs + [3]), f\"{len({1: 2})}\", len(len('ab') * 'c'))
    ";
    assert_eq!(
        fixed(program),
        dedent(
            "
            def main(stdout: Stdout):
                xs = [1, 2]
                stdout.print(xs.len(), (xs + [3]).len(), f\"{({1: 2}).len()}\", ('ab'.len() * 'c').len())
            "
        )
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(len([1]))"),
        [
            "the global functions are `sum`, `min`, `max`, `any`, `all`, `sorted`, `zip`, `enumerate` and `range`"
        ]
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(sorted(['a'], key=len))"),
        ["a length is a method: write `x.len()`, or `x => x.len()` to pass it"]
    );
}

#[test]
fn abs_is_a_method_and_its_fix_moves_it_there() {
    let program = "
        def main(stdout: Stdout):
            x = -2
            stdout.print(abs(x), abs(-3), abs(x * 2 if x > 0 else 1), abs(x).abs())
    ";
    assert_eq!(
        fixed(program),
        dedent(
            "
            def main(stdout: Stdout):
                x = -2
                stdout.print(x.abs(), (-3).abs(), (x * 2 if x > 0 else 1).abs(), x.abs().abs())
            "
        )
    );
    // The error doesn't hide what the value is.
    assert_eq!(
        problems_as_written("def main(stdout: Stdout):\n    stdout.print(abs(1) + 'a')"),
        ["`abs` is a method: write `x.abs()` @ abs(1)", "can't use `+` on an int and a str @ abs(1) + \"a\""]
    );
    assert_eq!(
        problems_as_written("def main(stdout: Stdout):\n    stdout.print(abs(1.5) + 1, len('a') + 'b')"),
        [
            "`abs` is a method: write `x.abs()` @ abs(1.5)",
            "can't mix int and float in `+` @ abs(1.5) + 1",
            "`len` is a method: write `x.len()` @ len(\"a\")",
            "can't use `+` on an int and a str @ len(\"a\") + \"b\"",
        ]
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(sorted([1], key=abs))"),
        ["a size is a method: write `x.abs()`, or `x => x.abs()` to pass it"]
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

        def word(n: int) -> str:
            if n > 2: fail TooBig(limit=2)
            \"\"

        def all(n: int) -> list[str]:
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
fn a_bare_return_cant_stand_for_a_value() {
    let problems = problems_as_written(
        "
        def f(n: int) -> int:
            if n > 0:
                return
            n
    ",
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("but this gives back nothing"), "{problems:?}");
    assert_eq!(problems_as_written("def g(n: int):\n    if n > 0:\n        return\n"), Vec::<String>::new());
}
