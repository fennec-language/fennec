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
