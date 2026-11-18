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

/// Two generators that can fail, each in its own way, for the tests of where one can go.
const FAILING: &str = "error Shut:\n    name: str\n\nerror Wrapped:\n    why: str\n\ndef first(name: str) -> Iterator[int] fails Shut:\n    yield 1\n    fail Shut(name=name)\n\ndef second(why: str) -> Iterator[int] fails Wrapped:\n    yield 2\n    fail Wrapped(why=why)\n\n";

/// What the checker says about `rest` after the two failing generators.
fn with_failing(rest: &str) -> Vec<String> {
    problems(&format!("{FAILING}{rest}"))
}

#[test]
fn a_generator_that_can_fail_is_looped_over_in_the_function_that_got_it() {
    let empty: [&str; 0] = [];
    for rest in [
        // Named, and looped over later, in a loop, a comprehension or `list`.
        "def f(stdout: Stdout) fails Shut | Wrapped:\n    kept = first(\"a\")?\n    for each in kept:\n        stdout.print(each)\n    stdout.print(second(\"b\")?)\n",
        "def f(stdout: Stdout) fails Shut:\n    kept = first(\"a\")?\n    stdout.print([each * 2 for each in kept])\n",
        "def f(stdout: Stdout) fails Shut:\n    kept = first(\"a\")?\n    stdout.print(list(kept))\n",
        "def f(stdout: Stdout) fails Shut:\n    stdout.print(\", \".join(f\"{each}\" for each in first(\"a\")?))\n",
        // Given again one that fails the same way.
        "def f(stdout: Stdout) fails Shut:\n    mut kept = first(\"a\")?\n    kept = first(\"b\")?\n    stdout.print(sum(kept))\n",
        // Kept in a list here, or through a generic function, and looped over here.
        "def f(stdout: Stdout) fails Shut:\n    held = [first(\"a\")?]\n    for kept in held:\n        stdout.print(sum(kept))\n",
        "def same[T](value: T) -> T:\n    return value\n\ndef f(stdout: Stdout) fails Shut:\n    stdout.print(sum(same(first(\"a\")?)))\n",
        // A generator kept for later over held ones, which fails as they do, looped over here.
        "def f(stdout: Stdout) fails Shut:\n    held = [first(\"a\")?]\n    later = (sum(gen) for gen in held)\n    stdout.print(sum(later))\n",
        // A generator looping over one, which fails as it does.
        "def outer() -> Iterator[int] fails Shut:\n    for each in first(\"a\")?:\n        yield each * 10\n\ndef f(stdout: Stdout) fails Shut:\n    stdout.print(sum(outer()?))\n",
        // `? from`, and `zip` or `enumerate`, right in a loop's own.
        "def f(stdout: Stdout) fails Wrapped:\n    for each in first(\"a\")? from Wrapped(why=\"w\"):\n        stdout.print(each)\n",
        "def f(stdout: Stdout) fails Shut:\n    for left, right in zip([1, 2], first(\"a\")?):\n        stdout.print(left, right)\n",
        "def f(stdout: Stdout) fails Shut | Wrapped:\n    for at, each in enumerate(second(\"a\")? from Wrapped(why=\"w\")):\n        stdout.print(at, each)\n",
    ] {
        assert_eq!(with_failing(rest), empty, "{rest}");
    }
}

#[test]
fn a_generator_that_can_fail_can_t_go_where_its_failure_wouldn_t_pass_up() {
    for (rest, said) in [
        (
            "def walk(items: Iterator[int]) -> int:\n    return sum(items)\n\ndef f(stdout: Stdout) fails Shut:\n    stdout.print(walk(first(\"a\")?))\n",
            "`items` should be an Iterator[int], but this is an Iterator[int] that fails with Shut @ first(\"a\")?",
        ),
        (
            "def get() -> Iterator[int] fails Shut:\n    return first(\"a\")?\n\ndef f(stdout: Stdout) fails Shut:\n    stdout.print(sum(get()?))\n",
            "`get` returns an Iterator[int], but this is an Iterator[int] that fails with Shut @ first(\"a\")?",
        ),
        (
            "struct Kept:\n    items: Iterator[int]\n\ndef f(stdout: Stdout) fails Shut:\n    kept = Kept(items=first(\"a\")?)\n    stdout.print(sum(kept.items))\n",
            "`items` should be an Iterator[int], but this is an Iterator[int] that fails with Shut @ first(\"a\")?",
        ),
        (
            "def f(stdout: Stdout, pick: bool) fails Shut | Wrapped:\n    mut kept = first(\"a\")?\n    if pick:\n        kept = second(\"b\")?\n    stdout.print(sum(kept))\n",
            "`kept` holds an Iterator[int] that fails with Shut, so it can't be given an Iterator[int] that fails with Wrapped @ kept",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    kept = first(\"a\")?\n    def walked(out: Stdout):\n        for each in kept:\n            out.print(each)\n    walked(stdout)\n",
            "`kept` is a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ kept",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    kept = first(\"a\")?\n    count = () => sum(kept)\n    stdout.print(count())\n",
            "`kept` is a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ kept",
        ),
        // One held in a list, a tuple or a generic struct, used from around (#2097).
        (
            "def f(stdout: Stdout) fails Shut:\n    held = [first(\"a\")?]\n    count = () => sum(sum(gen) for gen in held)\n    stdout.print(count())\n",
            "`held` holds a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ held",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    pair = (1, first(\"a\")?)\n    def walked(out: Stdout):\n        out.print(sum(pair[1]))\n    walked(stdout)\n",
            "`pair` holds a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ pair",
        ),
        (
            "struct Box[T]:\n    item: T\n\ndef f(stdout: Stdout) fails Shut:\n    box = Box(item=first(\"a\")?)\n    count = () => sum(box.item)\n    stdout.print(count())\n",
            "`box` holds a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ box",
        ),
        // `parallel` over held ones, or reading one in its body.
        (
            "def f(stdout: Stdout) fails Shut:\n    held = [first(\"a\")?, first(\"b\")?]\n    stdout.print([sum(gen) for gen in parallel(held)])\n",
            "`parallel` hands each item to another task, so its items can't hold a generator that can fail @ held",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    kept = first(\"a\")?\n    stdout.print([sum(kept) + at for at in parallel(range(2))])\n",
            "`kept` is a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ kept",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    held = [first(\"a\")?]\n    mut totals = [0, 0]\n    for ref total in parallel(totals):\n        total = sum(held[0])\n    stdout.print(totals)\n",
            "`held` holds a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ held",
        ),
        // A generator kept for later over held ones fails as they do, so it can't go where one that
        // can't fail goes.
        (
            "def walk(items: Iterator[int]) -> int:\n    return sum(items)\n\ndef f(stdout: Stdout) fails Shut:\n    held = [first(\"a\")?]\n    stdout.print(walk((sum(gen) for gen in held)))\n",
            "`items` should be an Iterator[int], but this is an Iterator[int] that fails with Shut @ (sum(gen) for gen in held)",
        ),
        // `except` on a generator function's call, which can never run.
        (
            "def f(stdout: Stdout):\n    kept = first(\"a\") except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "making a generator runs none of its body, so this can't fail here and `except` would never run @ first(\"a\")",
        ),
        (
            "def f(stdout: Stdout) fails Wrapped:\n    kept = first(\"a\")? from Wrapped(why=\"w\")\n    stdout.print(sum(kept))\n",
            "`? from` wraps a generator's failure only where a `for` loops over it right there @ first(\"a\")",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    kept = first(\"a\")?\n    for left, right in zip([1, 2], kept):\n        stdout.print(left, right)\n",
            "a generator that can fail goes into `zip` only as `made()?` written right in a `for` loop's own `zip` @ kept",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    numbered = enumerate(first(\"a\")?)\n    for at, each in numbered:\n        stdout.print(at, each)\n",
            "a generator that can fail goes into `enumerate` only as `made()?` written right in a `for` loop's own `enumerate` @ first(\"a\")?",
        ),
    ] {
        assert_eq!(with_failing(rest), [said], "{rest}");
    }
}

#[test]
fn an_interface_s_generator_fails_exactly_as_its_types_own_do() {
    let interface = "interface Source:\n    def items(self) -> Iterator[int] fails Shut | Wrapped\n\n";
    let narrower = "struct Counter:\n    start: int\n\n    def items(self) -> Iterator[int] fails Shut:\n        yield self.start\n        fail Shut(name=\"counter\")\n\n";
    let used = "def total[T: Source](source: T) -> int fails Shut | Wrapped:\n    return sum(source.items()?)\n\ndef f(stdout: Stdout) fails Shut | Wrapped:\n    stdout.print(total(Counter(start=1))?)\n";
    let said = "`Counter` isn't a `Source`: it has no `items(self) -> Iterator[int] fails Shut | Wrapped`. Its `items` returns a result of Iterator[int] that fails with Shut @ total(Counter(start=1))";
    assert_eq!(with_failing(&format!("{interface}{narrower}{used}")), [said]);
    // One that fails just as the interface says fits.
    let exact = narrower.replace("fails Shut:", "fails Shut | Wrapped:");
    let empty: [&str; 0] = [];
    assert_eq!(with_failing(&format!("{interface}{exact}{used}")), empty);
}
