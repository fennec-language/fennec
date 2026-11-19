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
        // `except` on a `def`'s call, generic or not: what it gives back holding one was made, and
        // counted, here.
        "error Bad:\n    why: str\n\ndef checked[T](value: T, text: str) -> T fails Bad:\n    if text == \"\":\n        fail Bad(why=\"empty\")\n    return value\n\ndef f(stdout: Stdout) fails Shut:\n    kept = checked(first(\"a\")?, \"x\") except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
        // `except` on a `def`'s call given a function that makes no generator, as a rule (#2118).
        "error Bad:\n    why: str\n\ndef checked[T](value: T, rule: (int) -> bool) -> T fails Bad:\n    if not rule(3):\n        fail Bad(why=\"no\")\n    return value\n\ndef f(stdout: Stdout) fails Shut:\n    kept = checked(first(\"a\")?, rule=(at) => at > 0) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
        // A function by name as the rule, and an arrow that loops over one inside and gives back a
        // list: neither gives back a generator, or a function that could make one.
        "error Bad:\n    why: str\n\ndef is_positive(at: int) -> bool:\n    return at > 0\n\ndef checked[T](value: T, rule: (int) -> bool) -> T fails Bad:\n    if not rule(3):\n        fail Bad(why=\"no\")\n    return value\n\ndef f(stdout: Stdout) fails Shut:\n    kept = checked(first(\"a\")?, rule=is_positive) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
        "error Bad:\n    why: str\n\ndef counted[T](value: T, rule: () -> list[int] fails Shut) -> T fails Bad | Shut:\n    if len(rule()?) == 0:\n        fail Bad(why=\"none\")\n    return value\n\ndef f(stdout: Stdout) fails Shut:\n    kept = counted(first(\"a\")?, rule=() => list(first(\"b\")?)) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
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

/// One held in a collection, a union or a generic struct, given to an arrow as its parameter, or
/// taken out of a call with `except` can't reach a loop whose function didn't count its failure
/// either (#2097).
#[test]
fn a_generator_that_can_fail_held_or_got_with_except_can_t_reach_a_loop_that_didn_t_count_it() {
    for (rest, said) in [
        // One held in a list, a tuple or a generic struct, used from around.
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
        // `except` on a generator function's call, which can never run, or a generator method's.
        (
            "def f(stdout: Stdout):\n    kept = first(\"a\") except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "making a generator runs none of its body, so this can't fail here and `except` would never run @ first(\"a\")",
        ),
        (
            "struct Source:\n    name: str\n\n    def items(self) -> Iterator[int] fails Shut:\n        yield 1\n        fail Shut(name=self.name)\n\ndef f(stdout: Stdout):\n    source = Source(name=\"m\")\n    kept = source.items() except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "making a generator runs none of its body, so this can't fail here and `except` would never run @ source.items()",
        ),
        // `except` on an arrow's call that gives back one, which its `?` counted as the arrow's.
        (
            "def f(stdout: Stdout):\n    make = () => [first(\"a\")?]\n    held = make() except why:\n        stdout.print(why)\n        return\n    for gen in held:\n        stdout.print(sum(gen))\n",
            "this arrow function gives back a generator that can fail, and `except` on its call leaves nothing to pass that generator's failure up @ make()",
        ),
        // An arrow given one as its own parameter, by a builtin or a generic function.
        (
            "def f(stdout: Stdout) fails Shut:\n    held = [first(\"a\")?]\n    stdout.print(len(sorted(held, key=gen => sum(gen))))\n",
            "this arrow function's `gen` is given a generator that can fail, and its failure can't pass up from inside the arrow @ gen => sum(gen)",
        ),
        (
            "def apply[T](value: T, run: (T) -> int) -> int:\n    return run(value)\n\ndef f(stdout: Stdout) fails Shut:\n    stdout.print(apply(first(\"a\")?, gen => sum(gen)))\n",
            "this arrow function's `gen` is given a generator that can fail, and its failure can't pass up from inside the arrow @ gen => sum(gen)",
        ),
        // One given to an arrow before what says its type, or read from around before it's
        // known to hold one, is looked at again once the function's types are.
        (
            "def apply[T](run: (T) -> bool, value: T) -> bool:\n    return run(value)\n\ndef f(stdout: Stdout) fails Shut:\n    stdout.print(apply(gen => any(True for _ in gen), first(\"a\")?))\n",
            "this arrow function's `gen` is given a generator that can fail, and its failure can't pass up from inside the arrow @ gen => any(True for _ in gen)",
        ),
        (
            "def f(stdout: Stdout) fails Shut:\n    mut held = []\n    for round in range(2):\n        seen = () => [any(True for _ in gen) for gen in held]\n        held.append(first(\"a\")?)\n        stdout.print(round, seen())\n",
            "`held` holds a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ held",
        ),
        // A nested function reading one from around before it's known to hold one: the function
        // around it looks again once it knows.
        (
            "def f(stdout: Stdout) fails Shut:\n    mut held = []\n    for turn in range(2):\n        def walked(out: Stdout):\n            out.print([any(True for _ in gen) for gen in held])\n        walked(stdout)\n        held.append(first(\"a\")?)\n        stdout.print(turn)\n",
            "`held` holds a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ held",
        ),
        // A generator function whose only `yield` is inside an `except` block.
        (
            "error Bad:\n    why: str\n\ndef risky() fails Bad:\n    fail Bad(why=\"no\")\n\ndef made(name: str) -> Iterator[int] fails Shut:\n    risky() except why:\n        yield 0\n        fail Shut(name=name)\n\ndef f(stdout: Stdout):\n    kept = made(\"a\") except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "making a generator runs none of its body, so this can't fail here and `except` would never run @ made(\"a\")",
        ),
        // One in a union's member.
        (
            "struct Box[T]:\n    item: T\n\nstruct Empty:\n    why: str\n\ndef boxed[T](item: T) -> Box[T] | Empty:\n    return Box(item=item)\n\ndef f(stdout: Stdout) fails Shut:\n    held = boxed(first(\"a\")?)\n    def walked(out: Stdout):\n        out.print(held)\n    walked(stdout)\n",
            "`held` holds a generator that can fail, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it @ held",
        ),
    ] {
        assert_eq!(with_failing(rest), [said], "{rest}");
    }
}

/// `except` on a `def`'s call that gives back a generator that can fail, given a function that
/// could have made it, is refused: its failure was counted, if at all, where that function's `?`
/// is, not here (#2117, #2118).
#[test]
fn except_on_a_call_given_a_function_that_could_make_one_is_refused() {
    for (rest, said) in [
        // `except` on a `def`'s call given a function, which may make one: an arrow, or a
        // function by name.
        (
            "def run_it[T, E](run: () -> T fails E) -> T fails E:\n    return run()?\n\ndef f(stdout: Stdout):\n    kept = run_it(() => first(\"a\")?) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ run_it(() => first(\"a\")?)",
        ),
        (
            "def call_with[T, E](make: (str) -> T fails E, name: str) -> T fails E:\n    return make(name)?\n\ndef f(stdout: Stdout):\n    kept = call_with(first, \"a\") except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ call_with(first, \"a\")",
        ),
        // An arrow giving back a function that makes one, one in a list, or one in a list given
        // whole or held in a struct's field (#2118's review).
        (
            "def call_got[T, E](get: () -> (str) -> T fails E) -> T fails E:\n    make = get()\n    return make(\"a\")?\n\ndef f(stdout: Stdout):\n    kept = call_got(() => first) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ call_got(() => first)",
        ),
        (
            "def call_twice[T, E](get: () -> () -> T fails E) -> T fails E:\n    make = get()\n    return make()?\n\ndef f(stdout: Stdout):\n    kept = call_twice(() => () => first(\"a\")?) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ call_twice(() => () => first(\"a\")?)",
        ),
        (
            "def first_of[T, E](get: () -> list[(str) -> T fails E]) -> T fails E:\n    makers = get()\n    make = makers[0]\n    return make(\"a\")?\n\ndef f(stdout: Stdout):\n    kept = first_of(() => [first]) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ first_of(() => [first])",
        ),
        (
            "def run_first[T, E](makers: list[() -> T fails E]) -> T fails E:\n    make = makers[0]\n    return make()?\n\ndef f(stdout: Stdout):\n    kept = run_first([() => first(\"a\")?]) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ run_first([() => first(\"a\")?])",
        ),
        (
            "struct Maker[T]:\n    make: () -> T fails Shut\n\ndef made[T](maker: Maker[T]) -> T fails Shut:\n    make = maker.make\n    return make()?\n\ndef f(stdout: Stdout):\n    kept = made(Maker(make=() => first(\"a\")?)) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ made(Maker(make=() => first(\"a\")?))",
        ),
        // One held in a struct an arrow gives back.
        (
            "struct Box[T]:\n    item: T\n\ndef boxed_got[T, E](get: () -> Box[(str) -> T fails E]) -> T fails E:\n    box = get()\n    make = box.item\n    return make(\"a\")?\n\ndef f(stdout: Stdout):\n    kept = boxed_got(() => Box(item=first)) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ boxed_got(() => Box(item=first))",
        ),
        // An arrow taking a function, which could hand one out to it: counted, whatever it gives
        // back.
        (
            "error Bad:\n    why: str\n\ndef stepped[T](value: T, rule: ((int) -> int) -> bool) -> T fails Bad:\n    if not rule((at) => at + 1):\n        fail Bad(why=\"no\")\n    return value\n\ndef f(stdout: Stdout) fails Shut:\n    kept = stepped(first(\"a\")?, rule=(step) => step(1) > 0) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n",
            "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ stepped(first(\"a\")?, rule=(step) => step(1) > 0)",
        ),
    ] {
        assert_eq!(with_failing(rest), [said], "{rest}");
    }
}

/// A function giving back itself, given to a `def` whose call's `except` is looked at, is looked
/// into once rather than without end (#2118's review). Seen again, it counts as one that could
/// have made the generator, so the `except` is refused beside the program's own two errors.
#[test]
fn a_function_giving_back_itself_is_looked_into_once() {
    let rest = "error Bad:\n    why: str\n\ndef me():\n    return me\n\ndef checked[T](value: T, rule: () -> int) -> T fails Bad:\n    return value\n\ndef f(stdout: Stdout) fails Shut:\n    kept = checked(first(\"a\")?, me) except why:\n        stdout.print(why)\n        return\n    stdout.print(sum(kept))\n";
    let said = [
        "`me` gives back a function, so its return type has to be written @ def me()",
        "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go @ checked(first(\"a\")?, me)",
        "`rule` should be a () -> int, but this is a function @ me",
    ];
    assert_eq!(with_failing(rest), said, "{rest}");
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
