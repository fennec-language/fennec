//! A generator is used once: a second use is a compile error where it can be proved.

use crate::problems;

const COUNT: &str = "def count(n: int) -> Iterator[int]:\n    for i in range(n):\n        yield i\n\n";

fn in_function(body: &str) -> Vec<String> {
    problems(&format!("{COUNT}def f(stdout: Stdout, flag: bool):\n{body}"))
}

#[test]
fn a_second_use_on_every_way_on_is_a_compile_error() {
    let said = "this generator has already been used";
    for (body, at) in [
        (
            "    g = count(3)\n    for x in g:\n        stdout.print(x)\n    for y in g:\n        stdout.print(y)\n",
            "g",
        ),
        ("    g = count(3)\n    stdout.print(sum(g), list(g))\n", "g"),
        ("    a = count(3)\n    b = a\n    stdout.print(sum(a))\n    stdout.print(list(b))\n", "b"),
        (
            "    g = count(3)\n    doubled = (x * 2 for x in g)\n    stdout.print(sum(g), list(doubled))\n",
            "g",
        ),
        (
            "    g = count(3)\n    if flag:\n        stdout.print(sum(g))\n    else:\n        stdout.print(list(g))\n    stdout.print(sum(g))\n",
            "g",
        ),
    ] {
        assert_eq!(in_function(body), [format!("{said} @ {at}")], "{body}");
    }
}

#[test]
fn a_use_that_may_not_happen_or_a_new_generator_is_left_to_the_run() {
    let empty: [&str; 0] = [];
    for body in [
        "    g = count(3)\n    if flag:\n        stdout.print(sum(g))\n    stdout.print(list(g))\n",
        "    mut g = count(3)\n    stdout.print(sum(g))\n    g = count(2)\n    stdout.print(sum(g))\n",
        "    g = count(3)\n    stdout.print(g)\n    stdout.print(sum(g))\n",
        "    g = count(3)\n    for i in range(2):\n        stdout.print(sum(g))\n",
        "    g = count(3)\n    if flag:\n        return\n    stdout.print(sum(g))\n",
    ] {
        assert_eq!(in_function(body), empty, "{body}");
    }
}

#[test]
fn a_generator_passed_in_is_used_once_there_too() {
    let empty: [&str; 0] = [];
    let total = "def total(numbers: Iterator[int]) -> int:\n    return sum(numbers)\n\n";
    let passed = format!("{COUNT}{total}def f(stdout: Stdout):\n    stdout.print(total(count(3)))\n");
    assert_eq!(problems(&passed), empty, "passing one down to be looped there is its one use");
    let twice = format!(
        "{COUNT}def twice(numbers: Iterator[int]) -> int:\n    first = sum(numbers)\n    return first + sum(numbers)\n"
    );
    assert_eq!(problems(&twice), ["this generator has already been used @ numbers"]);
}
