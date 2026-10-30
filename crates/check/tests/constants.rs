//! Constants: names at the top level of a file, computed before the program runs from
//! literals, collections and calls to functions that take no capabilities. Nothing changes
//! them, and none can depend on itself.

use fennec_check::Type;

use crate::{problems, returns};

#[test]
fn constants_have_the_type_of_their_value() {
    let program = "
        KEYWORDS = {'def', 'if', 'else'}
        LIMIT = 10
        TWICE: int = LIMIT * 2
        SQUARES = [n * n for n in range(LIMIT)]
        FIRST = square(3)

        def square(n: int) -> int:
            n * n

        def main(stdout: Stdout):
            stdout.print('if' in KEYWORDS, TWICE + 1, SQUARES[0] + FIRST)
            stdout.print(LIMIT + 'a', KEYWORDS + 1)

        def limit() -> int:
            LIMIT
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on an int and a str @ LIMIT + \"a\"",
            "can't use `+` on a set and an int @ KEYWORDS + 1"
        ]
    );
    assert_eq!(returns("LIMIT = 10\n\ndef limit() -> int:\n    LIMIT\n", "limit"), Ok(Type::Int));
}

#[test]
fn a_constant_written_with_a_type_has_that_type() {
    assert_eq!(problems("LIMIT: float = 10\n"), ["`LIMIT` should be a float, but this is an int @ 10"]);
    assert_eq!(
        problems("NAMES: list[str] = []\n\ndef main(stdout: Stdout):\n    stdout.print(NAMES)\n"),
        Vec::<String>::new()
    );
    assert_eq!(problems("NAMES = []\n"), ["can't tell what this empty list holds @ []"]);
}

#[test]
fn a_constant_cant_depend_on_itself() {
    let program = "
        WIDTH = HEIGHT * 2
        HEIGHT = WIDTH / 2
        SELF = SELF
    ";
    assert_eq!(
        problems(program),
        ["`HEIGHT` depends on itself @ HEIGHT = WIDTH / 2", "`SELF` depends on itself @ SELF = SELF",]
    );
}

#[test]
fn nothing_changes_a_constant() {
    let program = "
        KEYWORDS = {'def', 'if'}
        LIMIT = 10

        def main(stdout: Stdout):
            KEYWORDS.add('while')
            LIMIT = 11
            LIMIT += 1
            for mut word in KEYWORDS:
                pass
            limit: int = 3
            mut copy = KEYWORDS
            copy.add('while')
    ";
    assert_eq!(
        problems(program),
        [
            "`KEYWORDS` is a constant, so it can't change @ KEYWORDS",
            "`LIMIT` is a constant, so it can't change @ LIMIT",
            "`LIMIT` is a constant, so it can't change @ LIMIT",
            "`KEYWORDS` is a constant, so it can't change @ KEYWORDS",
        ]
    );
}

#[test]
fn a_constant_can_be_hidden_by_a_parameter() {
    let program = "
        LIMIT = 10

        def clamp(LIMIT: str) -> str:
            LIMIT.upper()
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn a_constant_has_no_capabilities_to_pass() {
    let program = "
        SETTINGS = load()

        def load(fs: Fs) -> str:
            fs.read_text('settings.toml') else ''
    ";
    assert_eq!(problems(program), ["`load` is missing its `fs` argument @ load()"]);
}

#[test]
fn mut_at_the_top_level_is_a_mistake() {
    assert_eq!(
        problems("mut score = 0\n"),
        ["parse: there's no `mut` at the top level of a file, so there's no state shared between functions"]
    );
    assert_eq!(
        problems("print(1)\n"),
        ["parse: only declarations and constants can go at the top level of a file"]
    );
    assert_eq!(problems("a.b = 1\n"), ["parse: a constant is one name, like `LIMIT = 10`"]);
}
