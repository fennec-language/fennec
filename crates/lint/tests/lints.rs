//! What each lint flags, and what `fennec fix` makes of it.

use fennec_lint::{Lint, across_files, lint, positional_arguments, within_files};
use fennec_syntax::{Module, Sources, apply_fixes, parse_at};

/// A file to put beside each test's, with lints and definitions of its own, an `extend` among them.
const BESIDE: &str = "from std.math import sqrt\n\ndef helper(value: int) -> int:\n    return value + 1\n\n\
extend Point:\n    def norm(self) -> float:\n        x = 1\n        return sqrt(2.0)\n\n\
def f(x: int) -> int:\n    if x == True:\n        return 1\n    return 2\n";

/// What a lint says and where, to compare lints found two ways.
fn said(lints: &[Lint]) -> Vec<(u32, u32, &'static str, String)> {
    let mut said: Vec<_> = lints
        .iter()
        .map(|lint| {
            (lint.diagnostic.span.start, lint.diagnostic.span.end, lint.rule, lint.diagnostic.message.clone())
        })
        .collect();
    said.sort();
    said
}

/// Holds that the lints of one file at a time find in each of two files, linted on its own, what
/// linting both together finds there, with the source first and then second.
fn alone_as_together(source: &str) {
    for files in
        [[("game.fen", source), ("beside.fen", BESIDE)], [("beside.fen", BESIDE), ("game.fen", source)]]
    {
        let mut sources = Sources::new();
        let mut module = Module::default();
        for (path, text) in files {
            let base = sources.add(path, text);
            let Ok(parsed) = parse_at(text, base) else { return };
            module.items.extend(parsed.items);
        }
        let mut parts = across_files(&module, &sources);
        for (path, _) in files {
            parts.extend(within_files(&module, &sources, false, &|wanted| wanted == path));
        }
        assert_eq!(said(&parts), said(&lint(&module, &sources)), "linting {source:?} beside another file");
    }
}

fn found(source: &str) -> Result<Vec<Lint>, String> {
    let mut sources = Sources::new();
    let base = sources.add("game.fen", source);
    let module = parse_at(source, base).map_err(|problem| problem.message)?;
    alone_as_together(source);
    // Positional arguments are a compile error now, kept with the lints they began as.
    let mut found = lint(&module, &sources);
    found.extend(positional_arguments(&module, &sources));
    Ok(found)
}

/// The rules about names, which most tests' short names would set off: `name_rules` has them.
const NAME_RULES: [&str; 3] = ["short-name", "naming-case", "shadowed-builtin"];

/// The rule about parameters, which most tests' small functions would set off: `fixed_params`
/// makes its fixes.
const PARAM_RULES: [&str; 1] = ["unused-param"];

/// The rule about items nothing uses, which most tests' small programs would set off too, since
/// they define a function and nothing calls it.
const ITEM_RULES: [&str; 1] = ["unused-item"];

/// The lints' names, which won't match anything a test expects if the source doesn't parse.
fn all_rules(source: &str) -> Vec<&'static str> {
    found(source)
        .map_or_else(|_| vec!["it doesn't parse"], |found| found.iter().map(|lint| lint.rule).collect())
}

fn rules(source: &str) -> Vec<&'static str> {
    all_rules(source)
        .into_iter()
        .filter(|rule| {
            !NAME_RULES.contains(rule) && !PARAM_RULES.contains(rule) && !ITEM_RULES.contains(rule)
        })
        .collect()
}

fn name_rules(source: &str) -> Vec<&'static str> {
    all_rules(source).into_iter().filter(|rule| NAME_RULES.contains(rule)).collect()
}

/// The source with every fix made but `PARAM_RULES`' and `ITEM_RULES`', which has to parse.
fn fixed(source: &str) -> String {
    fixed_by(source, &|rule| !PARAM_RULES.contains(&rule) && !ITEM_RULES.contains(&rule))
}

/// The source with only `PARAM_RULES`' fixes made.
fn fixed_params(source: &str) -> String {
    fixed_by(source, &|rule| PARAM_RULES.contains(&rule))
}

fn fixed_by(source: &str, made: &dyn Fn(&str) -> bool) -> String {
    let lints = found(source).unwrap_or_default();
    let diagnostics: Vec<_> =
        lints.into_iter().filter(|lint| made(lint.rule)).map(|lint| lint.diagnostic).collect();
    let fixed = apply_fixes(source, 0, &diagnostics).unwrap_or_default();
    if fennec_syntax::parse(&fixed).is_err() { format!("this doesn't parse: {fixed}") } else { fixed }
}

/// No lints, as `rules` gives them.
const NONE: [&str; 0] = [];

fn in_function(body: &str) -> String {
    format!("def f(a: int, ready: bool, x: int?) -> bool:\n    {body}\n")
}

#[test]
fn comparing_with_a_bool_or_none_has_a_plain_form() {
    for (written, plain) in [
        ("ready == True", "ready"),
        ("ready == False", "not ready"),
        ("ready != True", "not ready"),
        ("ready != False", "ready"),
    ] {
        let source = in_function(written);
        assert_eq!(rules(&source), ["bool-compare"], "{written}");
        assert_eq!(fixed(&source), in_function(plain), "{written}");
    }
    for (written, plain) in [("x == None", "x is None"), ("x != None", "x is not None")] {
        assert_eq!(rules(&in_function(written)), ["none-compare"]);
        assert_eq!(fixed(&in_function(written)), in_function(plain));
    }
    assert_eq!(rules(&in_function("a == 1")), NONE);
}

#[test]
fn only_what_is_surely_a_bool_loses_its_comparison_with_true() {
    let head = "def f(a: int, ready: bool, maybe: bool?, p: P, flags: list[bool]) -> bool:\n    ";
    for (written, plain) in [("ready == True", "ready"), ("ready != True", "not ready")] {
        let source = format!("{head}{written}\n");
        assert_eq!(rules(&source), ["bool-compare"], "{written}");
        assert_eq!(fixed(&source), format!("{head}{plain}\n"), "{written}");
    }
    // A `bool?` compared with `True` is `False` when it's `None`, which `maybe` alone isn't.
    for kept in [
        "maybe == True",
        "maybe != False",
        "p.alive == True",
        "any(ready == True for ready in flags)",
        "g(a) == True",
    ] {
        assert_eq!(rules(&format!("{head}{kept}\n")), NONE, "{kept}");
    }
}

#[test]
fn a_negated_comparison_says_the_opposite() {
    for (written, plain) in [
        ("not a == 1", "a != 1"),
        ("not a != 1", "a == 1"),
        ("not x is None", "x is not None"),
        ("not x is not None", "x is None"),
        ("not a in [1]", "a not in [1]"),
    ] {
        assert_eq!(rules(&in_function(written)), ["negated-compare"], "{written}");
        assert_eq!(fixed(&in_function(written)), in_function(plain), "{written}");
    }
    assert_eq!(rules(&in_function("not ready")), NONE);
    assert_eq!(rules(&in_function("not a < 1")), NONE, "NaN makes that not the same as `a >= 1`");
}

#[test]
fn an_operand_keeps_the_brackets_its_meaning_needs() {
    assert_eq!(rules(&in_function("(a > 1 or ready) == False")), NONE, "its span starts inside the bracket");
    assert_eq!(fixed(&in_function("not (a + 1) == 2")), in_function("(a + 1) != 2"));
}

#[test]
fn an_fstring_with_nothing_to_fill_in_is_a_string() {
    let source = "x = f\"ready\"\n";
    assert_eq!(rules(source), ["plain-fstring"]);
    assert_eq!(fixed(source), "x = \"ready\"\n");
    assert_eq!(rules("x = f\"{1}\"\n"), NONE);
    assert_eq!(rules("x = f\"{{\"\n"), NONE, "the braces would read differently");
}

#[test]
fn text_built_with_plus_and_str_is_one_fstring() {
    let source =
        "def f(hp: int, top: int, name: str) -> str:\n    \"HP: \" + str(hp) + \"/\" + str(top) + name\n";
    assert_eq!(rules(source), ["concat-to-fstring"], "one lint for the whole chain");
    assert_eq!(fixed(source), "def f(hp: int, top: int, name: str) -> str:\n    f\"HP: {hp}/{top}{name}\"\n");
    let source = "x = \"{a}\\n\\\"\" + str(1.5)\n";
    assert_eq!(
        fixed(source),
        "x = f\"{{a}}\\n\\\"{1.5}\"\n",
        "braces, escapes and quotes stay what they were"
    );
    assert_eq!(rules("x = \"a\" + name\n"), NONE, "no `str(...)` to save");
    assert_eq!(rules("x = str(a) + str(b)\n"), NONE, "nothing says the `+` joins text");
    assert_eq!(rules("x = \"a\" + str(d[\"k\"])\n"), NONE, "a quote can't go inside the braces");
    assert_eq!(rules("x = \"a\" + str(xs[1:2])\n"), NONE, "a `:` would start a format");
}

#[test]
fn a_path_to_a_name_imported_already_is_the_name() {
    let source = "import std.math\nfrom std.math import sqrt\n\ndef f(x: float) -> float:\n    std.math.sqrt(x) + sqrt(x)\n";
    assert!(rules(source).contains(&"full-path"), "{:?}", rules(source));
    assert_eq!(
        fixed(source),
        "import std.math\nfrom std.math import sqrt\n\ndef f(x: float) -> float:\n    sqrt(x) + sqrt(x)\n"
    );
    let aliased = "from std import json\nfrom std.json import loads as parse\n\ndef f(text: str):\n    json.loads(text)\n";
    assert!(fixed(aliased).contains("    parse(text)\n"), "{}", fixed(aliased));
    let renamed =
        "import game.physics as physics\nfrom game.physics import step\n\ndef f():\n    physics.step()\n";
    assert!(fixed(renamed).contains("    step()\n"), "{}", fixed(renamed));
    let other =
        "import std.math\nfrom std.math import sqrt\n\ndef f(x: float) -> float:\n    std.math.floor(x)\n";
    assert!(!rules(other).contains(&"full-path"), "a name that isn't imported by itself");
}

#[test]
fn assert_is_for_tests() {
    let source = "def heal(hp: int) -> int:\n    assert hp > 0\n    hp + 1\n";
    assert_eq!(rules(source), ["assert-outside-tests"]);
    let nested = "def f(n: int):\n    if n > 0:\n        match n:\n            case 1:\n                assert n == 1\n            case _:\n                pass\n";
    assert_eq!(rules(nested), ["assert-outside-tests"], "inside blocks and cases");
    let method = "struct Pool:\n    size: int\n\n    def check(self):\n        assert self.size > 0\n";
    assert_eq!(rules(method), ["assert-outside-tests"]);
    let test = "test \"adding\":\n    def twice(n: int) -> int:\n        assert n >= 0\n        n * 2\n    assert twice(2) == 4\n";
    assert_eq!(rules(test), NONE, "a test, and the functions in it");
}

#[test]
fn an_if_chain_comparing_one_name_with_values_is_a_match() {
    let source = "def name(n: int) -> str:\n    if n == 1:\n        \"one\"\n    elif 2 == n:\n        x = \"two\"\n        x\n    elif n == 3: \"three\"\n    else:\n        \"many\"\n";
    assert_eq!(rules(source), ["elif-to-match"]);
    assert_eq!(
        fixed(source),
        "def name(n: int) -> str:\n    match n:\n        case 1:\n            \"one\"\n        case 2:\n            x = \"two\"\n            x\n        case 3:\n            \"three\"\n        case _:\n            \"many\"\n"
    );
    let open = "def f(text: str):\n    if text == \"a\":\n        g()\n    elif text == \"b\":\n        h()\n    elif text == \"c\":\n        k()\n";
    assert!(
        fixed(open).ends_with("        case _:\n            pass\n"),
        "no `else` does nothing: {}",
        fixed(open)
    );
    let nested = "def f(n: int):\n    if n == 1:\n        if g():\n            h()\n    elif n == 2:\n        h()\n    elif n == 3:\n        k()\n    g()\n";
    assert_eq!(
        fixed(nested),
        "def f(n: int):\n    match n:\n        case 1:\n            if g():\n                h()\n        case 2:\n            h()\n        case 3:\n            k()\n        case _:\n            pass\n    g()\n",
        "a block inside keeps its lines, and what follows the chain stays on its own"
    );
    let two = "def f(n: int):\n    if n == 1:\n        g()\n    elif n == 2:\n        h()\n";
    assert_eq!(rules(two), NONE, "two branches read fine as they are");
    let mixed = "def f(n: int, m: int):\n    if n == 1:\n        g()\n    elif m == 2:\n        h()\n    elif n == 3:\n        k()\n";
    assert_eq!(rules(mixed), NONE, "two names");
    let commented = "def f(n: int):\n    if n == 1:\n        g()\n    # the rare one\n    elif n == 2:\n        h()\n    elif n == 3:\n        k()\n";
    assert_eq!(rules(commented), NONE, "a comment the rewrite would lose");
}

#[test]
fn a_parameter_nothing_reads_goes_with_its_arguments() {
    let source = "def damage(power: int, target: str, bonus: int = 1) -> int:\n    power * 2 + bonus\n\ndef main(stdout: Stdout):\n    stdout.print(damage(3, \"rat\"), damage(power=4, target=\"bat\", bonus=2))\n";
    assert!(all_rules(source).contains(&"unused-param"), "{:?}", all_rules(source));
    assert_eq!(
        fixed_params(source),
        "def damage(power: int, bonus: int = 1) -> int:\n    power * 2 + bonus\n\ndef main(stdout: Stdout):\n    stdout.print(damage(3), damage(power=4, bonus=2))\n"
    );
    // What comes after it starts at `ref`, not at its value.
    let source = "def f(unused: int, ref items: list[int]):\n    items.append(1)\n\ndef g():\n    mut xs = [1]\n    f(2, ref xs)\n";
    assert_eq!(
        fixed_params(source),
        "def f(ref items: list[int]):\n    items.append(1)\n\ndef g():\n    mut xs = [1]\n    f(ref xs)\n"
    );
    let last = "def f(n: int, unused: int) -> int:\n    n\n\ndef g() -> int:\n    f(1, 2)\n";
    assert_eq!(fixed_params(last), "def f(n: int) -> int:\n    n\n\ndef g() -> int:\n    f(1)\n");
    let held = "def f(n: int, unused: int) -> int:\n    n\n\ndef g(h: (int, int) -> int) -> int:\n    h(1, 2)\n\ndef k() -> int:\n    g(f)\n";
    assert!(!all_rules(held).contains(&"unused-param"), "held as a value, it has to take two");
    let public = "export def f(n: int, unused: int) -> int:\n    n\n";
    assert!(!all_rules(public).contains(&"unused-param"), "another kit may call it");
    let method = "struct Dog:\n    age: int\n\n    def speak(self, loud: bool) -> str:\n        \"woof\"\n";
    assert!(!all_rules(method).contains(&"unused-param"), "an interface may need it");
    let read_inside = "def f(n: int, scale: int) -> list[int]:\n    [x * scale for x in range(n)]\n";
    assert!(!all_rules(read_inside).contains(&"unused-param"), "read in a comprehension");
}

/// What a kit is held to beyond `lint`.
fn kit_rules(source: &str) -> Vec<&'static str> {
    let mut sources = Sources::new();
    let base = sources.add("game.fen", source);
    let Ok(module) = parse_at(source, base) else { return vec!["it doesn't parse"] };
    fennec_lint::kit_lints(&module).iter().map(|lint| lint.rule).collect()
}

#[test]
fn a_kit_gives_failures_back_rather_than_expecting() {
    let source = "def load(text: str) -> int:\n    int(text).expect(\"it's a number\")\n\nstruct Save:\n    level: int\n\n    def parsed(self, text: str) -> int:\n        def inner() -> int:\n            int(text).expect(\"a number\")\n        inner()\n";
    assert_eq!(
        kit_rules(source),
        ["expect-in-kit", "expect-in-kit"],
        "a function, and one nested in a method"
    );
    let main = "def main(stdout: Stdout):\n    stdout.print(int(\"1\").expect(\"it's a number\"))\n\ntest \"parses\":\n    assert int(\"2\").expect(\"a number\") == 2\n";
    assert_eq!(kit_rules(main), Vec::<&str>::new(), "`main` and tests decide for the program");
    let own = "struct Check:\n    ok: bool\n\n    def expect(self, why: str) -> bool:\n        self.ok\n\ndef f(check: Check) -> bool:\n    check.expect(\"fine\")\n";
    assert_eq!(kit_rules(own), Vec::<&str>::new(), "a method of its own named `expect`");
    assert_eq!(all_rules(source).iter().filter(|rule| **rule == "expect-in-kit").count(), 0, "only a kit");
}

/// How many conversions change nothing in a program, and the program with them taken out.
fn needless_conversions(source: &str) -> (usize, String) {
    let mut sources = Sources::new();
    let base = sources.add("game.fen", source);
    let Ok(module) = parse_at(source, base) else { return (0, "it doesn't parse".to_owned()) };
    let found = fennec_lint::needless_conversions(&module, &sources);
    let diagnostics: Vec<_> = found.iter().map(|lint| lint.diagnostic.clone()).collect();
    (found.len(), apply_fixes(source, 0, &diagnostics).unwrap_or_else(|| source.to_owned()))
}

#[test]
fn a_conversion_to_what_a_value_is_already_goes() {
    let text = "def f(name: str, n: int, x: float) -> str:\n    a = str(name)\n    b = int(n)\n    c = float(x)\n    f\"{a}{b}{c}\"\n";
    assert_eq!(
        needless_conversions(text),
        (3, "def f(name: str, n: int, x: float) -> str:\n    a = name\n    b = n\n    c = x\n    f\"{a}{b}{c}\"\n".to_owned())
    );
    let shown = "def f(n: int) -> str:\n    f\"{str(n)} left\"\n";
    assert_eq!(
        needless_conversions(shown),
        (1, "def f(n: int) -> str:\n    f\"{n} left\"\n".to_owned()),
        "an f-string shows anything"
    );
    let bracketed = "def f(a: str, b: str) -> int:\n    str(a + b).len()\n";
    assert_eq!(
        needless_conversions(bracketed),
        (1, "def f(a: str, b: str) -> int:\n    (a + b).len()\n".to_owned())
    );
    let converts = "def f(n: int, x: float) -> str:\n    y = int(x)\n    str(n + y)\n";
    assert_eq!(needless_conversions(converts).0, 0, "these change what the value is");
    let formatted = "def f(n: float) -> str:\n    f\"{str(n):>8}\"\n";
    assert_eq!(needless_conversions(formatted).0, 0, "a format reads the text, not the number");
    let own = "def str(n: int) -> int:\n    n + 1\n\ndef f(n: int) -> int:\n    str(n)\n";
    assert_eq!(needless_conversions(own).0, 0, "the program's own `str`");
}

#[test]
fn a_conversion_that_makes_a_value_of_a_names_own_stays() {
    let named = "def f(s: str) -> str:\n    mut y = str(s)\n    y += \"!\"\n    y\n";
    assert_eq!(needless_conversions(named).0, 0, "`mut y = s` would make `y` stand for `s`");
    let in_a_case = "def f(s: str, n: int) -> str:\n    match n:\n        case 1:\n            mut y = str(s)\n            y += \"!\"\n            y\n        case _:\n            s\n";
    assert_eq!(needless_conversions(in_a_case).0, 0, "in a case of a `match` too");
    let test_body = "test \"a name of its own\":\n    s = \"a\"\n    mut y = str(s)\n    y += \"!\"\n    assert s == \"a\"\n";
    assert_eq!(needless_conversions(test_body).0, 0, "in a test too");
    let in_a_loop = "def f(n: int) -> int:\n    mut h = \"a\"\n    mut total = 0\n    for _ in range(n):\n        k = str(h)\n        h += \"b\"\n        total += k.len()\n    total\n";
    let (count, fixed) = needless_conversions(in_a_loop);
    assert_eq!(
        (count, fixed),
        (1, in_a_loop.replace("str(h)", "h")),
        "text changes without a copy each pass"
    );
}

/// `double-lookup`'s findings in a source, and the source with its fixes made until none are left.
fn double_lookups(source: &str) -> (usize, String) {
    let mut text = source.to_owned();
    let mut first = None;
    for _ in 0..4 {
        let mut sources = Sources::new();
        let base = sources.add("game.fen", text.as_str());
        let Ok(module) = parse_at(&text, base) else { return (0, "it doesn't parse".to_owned()) };
        let diagnostics: Vec<_> =
            fennec_lint::double_lookups(&module, &sources).into_iter().map(|lint| lint.diagnostic).collect();
        first.get_or_insert(diagnostics.len());
        match apply_fixes(&text, base, &diagnostics) {
            Some(next) if next != text => text = next,
            _ => break,
        }
    }
    (first.unwrap_or(0), text)
}

#[test]
fn a_key_looked_up_after_asking_for_it_is_looked_up_once() {
    let wrap = |expr: &str| format!("def f(counts: dict[str, int], key: str) -> int:\n    {expr}\n");
    for (written, fixed) in [
        ("counts[key] if key in counts else 0", "counts.get(key) else 0"),
        ("0 if key not in counts else counts[key]", "counts.get(key) else 0"),
        ("counts[\"a\"] if \"a\" in counts else 1 + 2", "counts.get(\"a\") else 1 + 2"),
    ] {
        assert_eq!(double_lookups(&wrap(written)), (1, wrap(fixed)), "{written}");
    }
    let field = "struct Tally:\n    counts: dict[str, int]\n\n    def of(self, key: str) -> int:\n        self.counts[key] if key in self.counts else 0\n";
    assert_eq!(
        double_lookups(field),
        (1, field.replace("self.counts[key] if key in self.counts else 0", "self.counts.get(key) else 0")),
    );
    let none =
        "def f(counts: dict[str, int], key: str) -> int?:\n    counts[key] if key in counts else None\n";
    assert_eq!(
        double_lookups(none),
        (1, none.replace("counts[key] if key in counts else None", "counts.get(key)")),
        "a missing value stays one"
    );
    let two = "def f(one: dict[str, int], other: dict[str, int], key: str) -> int:\n    one[key] if key in one else other[key] if key in other else 0\n";
    assert_eq!(
        double_lookups(two).1,
        two.replace(
            "one[key] if key in one else other[key] if key in other else 0",
            "one.get(key) else other.get(key) else 0"
        ),
        "one inside another"
    );
}

#[test]
fn a_lookup_that_would_mean_something_else_stays() {
    for source in [
        // A list's `in` asks about its values, not its positions.
        "def f(xs: list[int], at: int) -> int:\n    xs[at] if at in xs else 0\n",
        // Another key, or another table.
        "def f(counts: dict[str, int], key: str, other: str) -> int:\n    counts[other] if key in counts else 0\n",
        "def f(one: dict[str, int], two: dict[str, int], key: str) -> int:\n    one[key] if key in two else 0\n",
        // A call may give something else the second time.
        "def next_key() -> str:\n    \"a\"\n\ndef f(counts: dict[str, int]) -> int:\n    counts[next_key()] if next_key() in counts else 0\n",
        // A type of its own may mean anything by these.
        "struct Odd:\n    size: int\n\n    def in(self, key: int) -> bool:\n        key < self.size\n\n    def [](self, key: int) -> int:\n        key\n\ndef f(odd: Odd, key: int) -> int:\n    odd[key] if key in odd else 0\n",
        // A bracket the span leaves out.
        "def f(counts: dict[str, int], key: str, more: int) -> int:\n    counts[key] if key in counts else (more + 1)\n",
        // The index runs where the key isn't there, which stops the program, as `get` wouldn't.
        "def f(counts: dict[str, int], key: str) -> int:\n    counts[key] if key not in counts else 0\n",
        "def f(counts: dict[str, int], key: str) -> int:\n    0 if key in counts else counts[key]\n",
    ] {
        let module = parse_at(source, 0).map_err(|problem| problem.message).unwrap();
        assert!(fennec_check::check_typed(&module).is_ok(), "it has to check to say anything: {source}");
        assert_eq!(double_lookups(source), (0, source.to_owned()), "{source}");
    }
}

/// `self-compare`'s messages in a source.
fn self_compares(source: &str) -> Vec<String> {
    let mut sources = Sources::new();
    let base = sources.add("game.fen", source);
    let Ok(module) = parse_at(source, base) else { return vec!["it doesn't parse".to_owned()] };
    assert!(fennec_check::check_typed(&module).is_ok(), "it has to check to say anything: {source}");
    fennec_lint::self_compares(&module, &sources).iter().map(|lint| lint.message().to_owned()).collect()
}

#[test]
fn comparing_a_value_with_itself_is_a_mistake() {
    for (ty, op, always) in [
        ("int", "==", "True"),
        ("str", "!=", "False"),
        ("bool", "==", "True"),
        // `==` treats NaN as equal to NaN, so this doesn't rule one out. `!=` is the checker's.
        ("float", "==", "True"),
    ] {
        let source = format!("def f(amount: {ty}) -> bool:\n    amount {op} amount\n");
        assert_eq!(self_compares(&source), [format!("`amount {op} amount` is always {always}")], "{source}");
    }
    let field = "struct Unit:\n    size: int\n\ndef f(unit: Unit) -> bool:\n    unit.size == unit.size\n";
    assert_eq!(self_compares(field), ["`unit.size == unit.size` is always True"]);
}

#[test]
fn comparing_what_may_differ_says_nothing() {
    for source in [
        "def f(one: int, other: int) -> bool:\n    one == other\n",
        "struct Unit:\n    size: int\n    cost: int\n\ndef f(unit: Unit) -> bool:\n    unit.size == unit.cost\n",
        // A call may give something else the second time.
        "def next_one() -> int:\n    1\n\ndef f() -> bool:\n    next_one() == next_one()\n",
        // A type of its own may mean anything by `==`.
        "struct Odd:\n    size: int\n\n    def ==(self, other: Odd) -> bool:\n        False\n\ndef f(odd: Odd) -> bool:\n    odd == odd\n",
        // An optional is something else, and so is any other comparison.
        "def f(maybe: int?) -> bool:\n    maybe == maybe\n",
        "def f(amount: int) -> bool:\n    amount <= amount\n",
    ] {
        assert_eq!(self_compares(source), Vec::<String>::new(), "{source}");
    }
}

#[test]
fn an_exported_item_says_what_it_is_for() {
    let missing = |source: &str| all_rules(source).iter().filter(|rule| **rule == "missing-doc").count();
    for source in [
        "export def spawn() -> int:\n    1\n",
        "export enum Mood:\n    Calm\n",
        "export type Size = int\n",
        "export LIMIT = 8\n",
        // Nothing but a `##` line just above it counts.
        "# A plain comment.\nexport def spawn() -> int:\n    1\n",
        "## A comment on nothing.\n\nexport def spawn() -> int:\n    1\n",
    ] {
        assert_eq!(missing(source), 1, "{source}");
    }
    // An exported type shows its fields and methods, apart from `_name` ones, and each needs a doc too.
    for source in [
        "export struct Unit:\n    size: int\n",
        "export error Lost:\n    reason: str\n",
        "export interface Named:\n    def name(self) -> str\n",
    ] {
        assert_eq!(missing(source), 2, "{source}");
    }
    let inside = "## A unit.\nexport struct Unit:\n    size: int\n    _hidden = 0\n\n    def grown(self) -> int:\n        self.size + 1\n";
    assert_eq!(missing(inside), 1, "its method: the type's doc covers its fields");
    let undocumented = "export struct Note:\n    title: str\n    body: str\n";
    assert_eq!(missing(undocumented), 3, "a type with no doc, and each of its fields");
}

#[test]
fn a_private_item_or_one_with_a_doc_says_nothing() {
    for source in [
        "def spawn() -> int:\n    1\n",
        "struct Unit:\n    size: int\n",
        "## Spawns one.\nexport def spawn() -> int:\n    1\n",
        "## Spawns one.\n## It's always the same one.\nexport def spawn() -> int:\n    1\n",
        "## Spawns one.\n@inline\nexport def spawn() -> int:\n    1\n",
        "## A unit.\nexport struct Unit:\n    ## How big it is.\n    size: int\n\n    ## How big it would be.\n    def grown(self) -> int:\n        self.size + 1\n",
        // A documented type covers its fields.
        "## A note someone wrote.\nexport struct Note:\n    title: str\n    body: str\n",
        // `export` inside text says nothing.
        "def spawn() -> str:\n    \"export def\"\n",
    ] {
        assert!(!all_rules(source).contains(&"missing-doc"), "{source}");
    }
}

#[test]
fn an_item_nothing_uses_goes_with_its_doc() {
    let source = "## Says hello.\ndef greet() -> str:\n    \"hi\"\n\n## A spare.\nstruct Spare:\n    size: int\n\n    def grow(self) -> int:\n        self.size + 1\n\ndef main(stdout: Stdout):\n    stdout.print(greet())\n";
    assert_eq!(
        all_rules(source).iter().filter(|rule| **rule == "unused-item").count(),
        1,
        "{:?}",
        all_rules(source)
    );
    assert_eq!(
        fixed_by(source, &|rule| rule == "unused-item"),
        "## Says hello.\ndef greet() -> str:\n    \"hi\"\n\ndef main(stdout: Stdout):\n    stdout.print(greet())\n"
    );
    let cases = "enum Mood:\n    Calm\n    Angry\n\ndef main(stdout: Stdout):\n    mood = .Calm\n    stdout.print(mood == .Angry)\n";
    assert!(!all_rules(cases).contains(&"unused-item"), "an enum used through its cases");
    let typed = "struct Point:\n    x: int\n\ndef origin(p: Point) -> int:\n    p.x\n\ndef main(stdout: Stdout):\n    stdout.print(1)\n";
    let flagged: Vec<String> = found(typed)
        .unwrap()
        .iter()
        .filter(|lint| lint.rule == "unused-item")
        .map(|lint| lint.message().to_owned())
        .collect();
    assert_eq!(flagged, ["nothing uses the function `origin`"], "a type named in a parameter is used");
    let public = "export def greet() -> str:\n    \"hi\"\n";
    assert!(!all_rules(public).contains(&"unused-item"), "another kit may use it");
    let recursive = "def countdown(n: int) -> int:\n    if n == 0: return 0\n    countdown(n - 1)\n\ndef main(stdout: Stdout):\n    stdout.print(1)\n";
    assert!(all_rules(recursive).contains(&"unused-item"), "calling itself isn't a use");
}

#[test]
fn a_use_is_a_whole_word_whatever_letters_are_around_it() {
    let unused = |source: &str| -> Vec<String> {
        found(source)
            .unwrap()
            .iter()
            .filter(|lint| lint.rule == "unused-item")
            .map(|lint| lint.message().to_owned())
            .collect()
    };
    // A letter that isn't ASCII is part of the word, so `éhelper` isn't `helper`.
    let inside =
        "def helper() -> int:\n    1\n\ndef main(stdout: Stdout):\n    # éhelper\n    stdout.print(2)\n";
    assert_eq!(unused(inside), ["nothing uses the function `helper`"]);
    // Marks that aren't letters end it, so `«helper»` is.
    let marked =
        "def helper() -> int:\n    1\n\ndef main(stdout: Stdout):\n    # «helper»\n    stdout.print(2)\n";
    assert!(unused(marked).is_empty(), "{:?}", unused(marked));
    // Names longer than 63 bytes share a length, and are still told apart by what they say.
    let long = "a".repeat(70);
    let longer = "b".repeat(80);
    let both = format!(
        "def {long}() -> int:\n    1\n\ndef {longer}() -> int:\n    2\n\ndef main(stdout: Stdout):\n    stdout.print({long}())\n"
    );
    assert_eq!(unused(&both), [format!("nothing uses the function `{longer}`")]);
}

#[test]
fn pass_is_only_for_an_empty_block() {
    assert_eq!(rules("def f():\n    pass\n"), NONE);
    let source = "def f():\n    pass\n    g()\n";
    assert_eq!(rules(source), ["needless-pass"]);
    assert_eq!(fixed(source), "def f():\n    \n    g()\n");
}

/// A `def` gives its value with `return`, its last line's too, so no lint flags one there or
/// takes it off.
#[test]
fn a_last_line_keeps_its_return() {
    for source in [
        "def f() -> int:\n    return 1\n",
        "def f(a: bool) -> int:\n    if a:\n        return 1\n    return 2\n",
        "def f():\n    g()\n    return\n",
    ] {
        assert_eq!(rules(source), NONE, "{source}");
    }
}

#[test]
fn sizes_have_limits() {
    let lines = "    g()\n".repeat(80);
    assert_eq!(rules(&format!("def f():\n{}", "    g()\n".repeat(79))), NONE, "80 lines is allowed");
    assert_eq!(rules(&format!("def f():\n{lines}")), ["long-function"]);

    assert_eq!(rules(&"x = 1\n".repeat(800)), NONE);
    assert_eq!(rules(&"x = 1\n".repeat(801)), ["long-file"]);

    let six = "a: int, b: str, c: int, d: str, e: int, f: str";
    assert_eq!(rules(&format!("def g({six}, out: Stdout):\n    pass\n")), NONE, "capabilities don't count");
    assert_eq!(rules(&format!("def g({six}, h: int):\n    pass\n")), ["too-many-params"]);

    let deep = |levels: usize| {
        let mut source = "def f(a: bool):\n".to_owned();
        for level in 1..=levels {
            source += &"    ".repeat(level);
            source += "if a:\n";
        }
        source += &"    ".repeat(levels + 1);
        source += "pass\n";
        source
    };
    assert_eq!(rules(&deep(4)), NONE);
    assert_eq!(rules(&deep(5)), ["deep-nesting"]);
}

const CALLEES: &str = "def spawn(name: str, hostile: bool):\n    pass\n\n\
    def between(low: int, high: int):\n    pass\n\n\
    def place(name: str, x: int, y: int):\n    pass\n\n\
    def one(a: int):\n    pass\n\n\
    def mixed(a: int, b: str):\n    pass\n\n\
    struct Point:\n    x: int\n    y: int\n\n    def moved(self, by: int, up: int):\n        pass\n\n";

fn calling(call: &str) -> String {
    format!("{CALLEES}def main():\n    {call}\n")
}

#[test]
fn an_argument_that_cant_be_read_from_the_call_needs_a_name() {
    for (call, named) in [
        ("spawn(\"fox\", True)", "spawn(\"fox\", hostile=True)"),
        ("between(1, 2)", "between(1, high=2)"),
        ("place(\"fox\", 1, 2)", "place(\"fox\", x=1, y=2)"),
        ("Point(1, 2)", "Point(1, y=2)"),
        ("p.moved(1, 2)", "p.moved(1, up=2)"),
    ] {
        assert_eq!(rules(&calling(call)), ["positional-arguments"], "{call}");
        assert_eq!(fixed(&calling(call)), calling(named), "{call}");
    }
}

/// What two arguments both take is named as it's written: `shared Account`, `list[int]`.
#[test]
fn a_swap_names_the_type_as_its_written() {
    let program = "struct Account:\n    balance = 0\n\n\
        def transfer(source: shared Account, target: shared Account):\n    pass\n\n\
        def both(kept: list[int], dropped: list[int]):\n    pass\n\ndef main():\n    transfer(x, y)\n    both(x, y)\n";
    let said: Vec<String> =
        found(program).unwrap().iter().map(|lint| lint.diagnostic.message.clone()).collect();
    assert!(
        said.iter()
            .any(|said| said.contains("`source` and `target` both take `shared Account`, so swapping")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|said| said.contains("`kept` and `dropped` both take `list[int]`, so swapping")),
        "{said:?}"
    );
}

#[test]
fn an_argument_that_can_be_read_stays_as_it_is() {
    for call in [
        "one(1)",
        "one(a=1)",
        "mixed(1, \"a\")",
        "between(low=1, high=2)",
        "between(1, high=2)",
        "len(1, 2, 3)",
    ] {
        assert_eq!(rules(&calling(call)), NONE, "{call}");
    }
}

/// `dot(a, b)`, `max(a, b)` and the like name their parameters for where they come and nothing
/// else, so naming one at the call would say no more than its place does. Coordinates mean
/// something, so they're still asked for.
#[test]
fn parameters_named_only_for_their_place_stay_positional() {
    let defs = "def dot(a: Vec3, b: Vec3) -> float:\n    0.0\n\ndef larger(a: int, b: int) -> int:\n    a\n\n\
        def mid(a: int, b: int, c: int) -> int:\n    a\n\ndef at(x: int, y: int):\n    pass\n\n";
    for call in ["dot(u, v)", "larger(1, 2)", "mid(1, 2, 3)"] {
        assert_eq!(rules(&format!("{defs}def main():\n    {call}\n")), NONE, "{call}");
    }
    assert_eq!(rules(&format!("{defs}def main():\n    at(1, 2)\n")), ["positional-arguments"]);
}

#[test]
fn three_arguments_of_types_that_cant_be_swapped_stay_positional() {
    let types = "struct World:\n    size: int\n\nstruct Board:\n    wide: int\n\n\
        interface Named:\n    def name(self) -> str\n\ntype Meters = int\n\n";
    let program = |def: &str, call: &str| format!("{types}{def}\n    pass\n\ndef main():\n    {call}\n");
    let report = "def report(out: Stdout, world: World, board: Board):";
    assert_eq!(rules(&program(report, "report(stdout, world, board)")), NONE);
    for (def, call) in [
        ("def report(out: Stdout, world: World, again: World):", "report(stdout, world, other)"),
        ("def place[T](at: T, x: int, label: str):", "place(1, 2, \"a\")"),
        ("def place(at: Named, x: World, label: str):", "place(world, world, \"a\")"),
        ("def place(at: Meters, x: int, label: str):", "place(1, 2, \"a\")"),
        ("def place(at: int?, x: int, label: str):", "place(1, 2, \"a\")"),
    ] {
        assert_eq!(rules(&program(def, call)), ["positional-arguments"], "{def}");
    }
}

#[test]
fn a_method_with_two_shapes_cant_be_told_apart_so_it_isnt_flagged() {
    let source = "struct A:\n    def go(self, x: int, y: int):\n        pass\n\n\
        struct B:\n    def go(self, x: str, y: str):\n        pass\n\n\
        def main():\n    a.go(1, 2)\n";
    assert_eq!(rules(source), NONE);
}

#[test]
fn an_enum_case_names_its_arguments_too() {
    let enumerated = "enum Shape:\n    Rect(wide: int, high: int)\n    Dot\n\n";
    for (call, named) in [("Shape.Rect(1, 2)", "Shape.Rect(1, high=2)"), (".Rect(1, 2)", ".Rect(1, high=2)")]
    {
        let source = format!("{enumerated}def main():\n    x = {call}\n");
        assert_eq!(rules(&source), ["positional-arguments"], "{call}");
        assert_eq!(fixed(&source), format!("{enumerated}def main():\n    x = {named}\n"), "{call}");
    }
}

#[test]
fn the_standard_librarys_types_count_unless_the_program_replaces_them() {
    let library = "def main():\n    x = Vec3(1.0, 2.0, 3.0)\n";
    assert_eq!(rules(library), ["positional-arguments"], "std.math's Vec3");
    assert_eq!(fixed(library), "def main():\n    x = Vec3(1.0, y=2.0, z=3.0)\n");
    let replaced = "def Vec3(a: int, b: str):\n    pass\n\ndef main():\n    x = Vec3(1, \"a\")\n";
    assert_eq!(rules(replaced), NONE, "the program's own parameters are what matter");
}

#[test]
fn a_name_a_function_binds_itself_isnt_flagged() {
    let source = "def between(low: int, high: int):\n    pass\n\n\
                  def main(between: (int, int) -> int):\n    between(1, 2)\n";
    assert_eq!(rules(source), NONE, "a parameter");
    let source = "def between(low: int, high: int):\n    pass\n\n\
                  def main():\n    between = other\n    between(1, 2)\n";
    assert_eq!(rules(source), NONE, "a local");
}

#[test]
fn code_after_leaving_a_block_cant_run() {
    let source = "def f(a: int) -> int:\n    return a\n    a + 1\n";
    assert!(rules(source).contains(&"unreachable"));
    assert_eq!(rules("def f():\n    while True:\n        break\n        g()\n"), ["unreachable"]);
    assert_eq!(
        fixed("def f():\n    while True:\n        break\n        g()\n"),
        "def f():\n    while True:\n        break\n        \n"
    );
}

#[test]
fn an_else_after_branches_that_leave_goes() {
    let head = "def f(a: int, xs: list[int]) -> int:\n";
    for (written, plain) in [
        (
            "    if a < 0:\n        return 0\n    else:\n        g(a)\n        a + 1\n",
            "    if a < 0:\n        return 0\n    g(a)\n    a + 1\n",
        ),
        (
            "    if a < 0:\n        fail Bad()\n    elif a == 0:\n        return 1\n    else:\n        for x in xs:\n            g(x)\n\n        a\n",
            "    if a < 0:\n        fail Bad()\n    elif a == 0:\n        return 1\n    for x in xs:\n        g(x)\n\n    a\n",
        ),
        ("    if a < 0: return 0\n    else: a\n", "    if a < 0: return 0\n    a\n"),
        (
            "    for x in xs:\n        if x < 0:\n            continue\n        else:\n            g(x)\n    a\n",
            "    for x in xs:\n        if x < 0:\n            continue\n        g(x)\n    a\n",
        ),
        (
            "    if a < 0:\n        if a < -9:\n            return 2\n        else:\n            return 1\n    else:\n        a\n",
            "    if a < 0:\n        if a < -9:\n            return 2\n        return 1\n    a\n",
        ),
    ] {
        let source = format!("{head}{written}");
        assert!(rules(&source).iter().all(|rule| *rule == "needless-else"), "{written}");
        assert!(rules(&source).contains(&"needless-else"), "{written}");
        assert_eq!(fixed(&source), format!("{head}{plain}"), "{written}");
    }
    for kept in [
        "    if a < 0:\n        g(a)\n    else:\n        return 1\n    a\n",
        "    if a < 0:\n        return 0\n    elif a == 0:\n        g(a)\n    else:\n        return 1\n    a\n",
        "    if a < 0:\n        return 0\n    else:  # the usual case\n        a\n",
        "    if a < 0:\n        return 0\n    a\n",
    ] {
        let source = format!("{head}{kept}");
        assert!(!rules(&source).contains(&"needless-else"), "{kept}");
    }
}

#[test]
fn an_if_that_only_gives_back_its_condition_is_its_condition() {
    for (written, plain) in [
        ("    if a > 0: return True\n    False\n", "    return a > 0\n"),
        ("    if a > 0: return False\n    return True\n", "    return not (a > 0)\n"),
        ("    if a > 0:\n        return True\n    else:\n        return False\n", "    return a > 0\n"),
    ] {
        let source = format!("def f(a: int) -> bool:\n{written}");
        assert!(rules(&source).contains(&"return-bool"), "{written}");
        assert_eq!(
            fixed(&source).replace("\n    \n", "\n"),
            format!("def f(a: int) -> bool:\n{plain}"),
            "{written}"
        );
    }
    assert_eq!(rules("def f(a: int) -> bool:\n    if a > 0: return True\n    True\n"), NONE);
}

/// A write to standard output or error inside a `parallel` loop's body is a hint: its tasks
/// print in whatever order they get there. It's found however deep in the body, by whatever name
/// the stream has, once even inside two loops, and never outside one or in a plain loop.
#[test]
fn printing_inside_a_parallel_loop_is_a_hint_about_order() {
    let head = "def main(stdout: Stdout, stderr: Stderr):\n";
    for body in [
        "    for x in parallel([1, 2]):\n        stdout.print(x)\n    return\n",
        "    for x in parallel([1, 2]):\n        if x > 1:\n            stderr.write(f\"{x}\")\n    return\n",
        "    for x in parallel([1, 2]):\n        for y in parallel([3, 4]):\n            stdout.print(x, y)\n    return\n",
    ] {
        let source = format!("{head}{body}");
        let found = found(&source).unwrap();
        let hints: Vec<_> = found.iter().filter(|lint| lint.rule == "parallel-print").collect();
        assert_eq!(hints.len(), 1, "{body}");
        assert_eq!(hints[0].severity, fennec_lint::Severity::Hint, "{body}");
        assert!(hints[0].diagnostic.message.contains("no fixed order"), "{}", hints[0].diagnostic.message);
        let help = hints[0].diagnostic.help.as_deref().unwrap_or_default();
        assert!(help.contains("for line in (render(x) for x in parallel(xs)): stdout.print(line)"), "{help}");
    }
    let renamed = "def main(out: Stdout):\n    for x in parallel([1, 2]):\n        out.print(x)\n    return\n";
    assert!(all_rules(renamed).contains(&"parallel-print"));
    for clean in [
        "    for x in [1, 2]:\n        stdout.print(x)\n    return\n",
        "    for line in (f\"{x}\" for x in parallel([1, 2])):\n        stdout.print(line)\n    return\n",
        "    total = sum([x for x in parallel([1, 2])])\n    stdout.print(total)\n    return\n",
    ] {
        let source = format!("{head}{clean}");
        assert!(!all_rules(&source).contains(&"parallel-print"), "{clean}");
    }
}

#[test]
fn a_condition_tested_twice_cant_run_the_second_time() {
    let source = "def f(a: int):\n    if a > 0:\n        g()\n    elif a < 0:\n        h()\n    elif a > 0:\n        i()\n";
    assert_eq!(rules(source), ["duplicate-condition"]);
    let calls = "def f():\n    if g():\n        h()\n    elif g():\n        i()\n";
    assert_eq!(rules(calls), NONE, "a call can give something else the second time");
}

#[test]
fn branches_that_do_the_same_are_flagged() {
    assert_eq!(
        rules("def f(a: bool):\n    if a:\n        g()\n    else:\n        g()\n"),
        ["identical-branches"]
    );
    assert_eq!(rules("def f(a: bool):\n    if a:\n        g()\n    else:\n        h()\n"), NONE);
}

#[test]
fn a_range_test_is_one_chain() {
    for (written, plain) in [("0 <= a and a <= 10", "0 <= a <= 10"), ("10 > a and a >= 0", "10 > a >= 0")] {
        assert_eq!(rules(&in_function(written)), ["chained-compare"], "{written}");
        assert_eq!(fixed(&in_function(written)), in_function(plain), "{written}");
    }
    assert_eq!(rules(&in_function("0 <= a and a >= 10")), NONE, "they point different ways");
    assert_eq!(rules(&in_function("0 <= a and x <= 10")), NONE, "they test different things");
    assert_eq!(rules(&in_function("0 <= a < 10")), NONE, "it's a chain already");
    assert_eq!(rules(&in_function("0 <= a < 10 and a > 2")), NONE, "and so is this, with more after");
}

#[test]
fn a_default_for_none_has_its_own_form() {
    for written in ["x if x is not None else 1", "1 if x is None else x"] {
        assert_eq!(rules(&in_function(written)), ["manual-default"], "{written}");
        assert_eq!(fixed(&in_function(written)), in_function("x else 1"), "{written}");
    }
    assert_eq!(rules(&in_function("a if x is not None else 1")), NONE);
}

#[test]
fn an_import_nothing_uses_goes() {
    let source = "from std.math import PI, sqrt\n\ndef f() -> float:\n    sqrt(2.0)\n";
    assert_eq!(rules(source), ["unused-import"]);
    assert_eq!(fixed(source), "from std.math import sqrt\n\ndef f() -> float:\n    sqrt(2.0)\n");
    let whole = "import std.json\nfrom std.math import PI\n\ndef f() -> float:\n    PI\n";
    assert_eq!(fixed(whole), "from std.math import PI\n\ndef f() -> float:\n    PI\n");
    let renamed = "from std.math import sqrt as root, PI\n\ndef f() -> float:\n    root(PI)\n";
    assert_eq!(rules(renamed), NONE);
}

#[test]
fn a_name_in_an_fstring_or_a_type_is_a_use() {
    assert_eq!(rules("from std.math import PI\n\ndef f() -> str:\n    f\"{PI}\"\n"), NONE);
    assert_eq!(rules("from std.math import Vec3\n\ndef f(v: Vec3) -> int:\n    1\n"), NONE);
    assert_eq!(rules("import std.json\n\ndef f() -> str:\n    std.json.dumps([1])\n"), NONE);
    assert_eq!(rules("from std import json\n\ndef f() -> str:\n    json.dumps([1])\n"), NONE);
}

#[test]
fn a_local_nothing_reads_keeps_what_its_value_does() {
    let source = "def f() -> int:\n    damage = attack()\n    1\n";
    assert_eq!(rules(source), ["unused-name"]);
    assert_eq!(fixed(source), "def f() -> int:\n    attack()\n    1\n");
    let read = [
        "def f() -> int:\n    damage = attack()\n    damage\n",
        "def f() -> int:\n    mut total = 0\n    total += 1\n    2\n",
        "def f() -> int:\n    xs = [1]\n    xs.append(2)\n    3\n",
        "def f() -> str:\n    name = \"a\"\n    f\"{name}\"\n",
        "def f() -> int:\n    n = 1\n    def g() -> int:\n        n\n    g()\n",
        "def f() -> int:\n    _skip = attack()\n    1\n",
    ];
    for source in read {
        assert_eq!(rules(source), NONE, "{source}");
    }
}

#[test]
fn a_ref_case_changes_what_the_match_is_over() {
    let kinds = "enum Kind:\n    Button(clicks: int)\n    Label\n\nstruct Widget:\n    kind: Kind\n\n";
    for (made, over) in [
        ("kind = Kind.Button(clicks=1)", "kind"),
        ("widgets = [Widget(kind=Kind.Button(clicks=1))]", "widgets[0].kind"),
    ] {
        let source = format!(
            "{kinds}def main(stdout: Stdout):\n    mut {made}\n    match {over}:\n        case .Button(ref clicks):\n            clicks += 1\n            stdout.print(clicks)\n        case .Label:\n            pass\n"
        );
        assert!(!rules(&source).contains(&"needless-mut"), "{over}");
    }
}

#[test]
fn mut_on_a_local_nothing_changes_goes() {
    let source = "def f() -> int:\n    mut speed = 4\n    speed + 1\n";
    assert_eq!(rules(source), ["needless-mut"]);
    assert_eq!(fixed(source), "def f() -> int:\n    speed = 4\n    speed + 1\n");
    let changed = [
        "def f() -> int:\n    mut n = 0\n    n += 1\n    n\n",
        "def f() -> int:\n    mut n = 0\n    n = 2\n    n\n",
        "def f() -> int:\n    mut xs = [1]\n    xs.append(2)\n    xs.len()\n",
        "def f() -> int:\n    mut xs = [1]\n    xs[0] = 2\n    xs[0]\n",
        "def f(p: P) -> int:\n    mut q = p\n    q.pos.x = 1\n    q.pos.x\n",
        "def f() -> int:\n    mut xs = [1]\n    g(ref xs)\n    xs[0]\n",
        "def f() -> int:\n    mut xs = [1]\n    for ref x in xs:\n        x += 1\n    xs[0]\n",
        "def f() -> int:\n    mut n = 0\n    def bump():\n        n += 1\n    bump()\n    n\n",
        "def f() -> int:\n    mut n = 0\n    m, n = pair()\n    m + n\n",
        "def f() -> int:\n    mut w = W(xs=[1])\n    mut part = w.xs\n    mut deeper = part\n    deeper[0] = 2\n    w.xs[0]\n",
        "def f(s: S) -> int:\n    mut at = s\n    match at:\n        case .A(inner):\n            at = inner\n        case _:\n            pass\n    1\n",
        "def f(s: S) -> int:\n    mut src = s\n    src.next[int]() else 0\n",
    ];
    for source in changed {
        assert_eq!(rules(source), NONE, "{source}");
    }
}

/// The source, with `fennec fix`'s blank lines left by removed lines taken out.
fn fixed_tidy(source: &str) -> String {
    let text = fixed(source);
    let kept: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    kept.join("\n") + "\n"
}

#[test]
fn an_unused_index_needs_no_enumerate() {
    let source = "def f(xs: list[int]):\n    for i, x in enumerate(xs):\n        g(x)\n";
    assert_eq!(rules(source), ["needless-enumerate"]);
    assert_eq!(fixed(source), "def f(xs: list[int]):\n    for x in xs:\n        g(x)\n");
    assert_eq!(rules("def f(xs: list[int]):\n    for i, x in enumerate(xs):\n        g(i, x)\n"), NONE);
}

#[test]
fn a_loop_that_leaves_with_else_stays_a_loop() {
    let source = "def f(texts: list[str]) -> list[int]:\n    mut found = []\n    for text in texts:\n        found.append(int(text) else continue)\n    found\n";
    assert_eq!(rules(source), NONE);
}

#[test]
fn a_loop_that_only_appends_is_a_comprehension() {
    let source = "def f(ps: list[P]) -> list[str]:\n    mut names = []\n    for p in ps:\n        if p.alive:\n            names.append(p.name)\n    names\n";
    assert_eq!(rules(source), ["manual-comprehension"]);
    assert_eq!(
        fixed(source),
        "def f(ps: list[P]) -> list[str]:\n    mut names = [p.name for p in ps if p.alive]\n    names\n"
    );
    let more = "def f(ps: list[P]) -> list[str]:\n    mut names = []\n    for p in ps:\n        names.append(p.name)\n        g()\n    names\n";
    assert_eq!(rules(more), NONE, "the loop does something else too");
    let started = "def f(ps: list[P]) -> list[str]:\n    mut names = [\"a\"]\n    for p in ps:\n        names.append(p.name)\n    names\n";
    assert_eq!(rules(started), NONE, "it didn't start empty");
    let bracketed = "def f(ps: list[int]) -> list[int]:\n    mut out = []\n    for p in ps:\n        out.append(p + (p if p > 0 else 0))\n    out\n";
    assert_eq!(rules(bracketed), NONE, "the span leaves out a bracket it needs");
}

#[test]
fn a_loop_that_only_adds_is_a_sum() {
    let source = "def f(cart: list[I]) -> int:\n    mut total = 0\n    for item in cart:\n        total += item.price\n    total\n";
    assert_eq!(rules(source), ["manual-builtin"]);
    assert_eq!(
        fixed(source),
        "def f(cart: list[I]) -> int:\n    mut total = sum(item.price for item in cart)\n    total\n"
    );
    let uses_total = "def f(cart: list[I]) -> int:\n    mut total = 0\n    for item in cart:\n        total += total\n    total\n";
    assert_eq!(rules(uses_total), NONE);
}

#[test]
fn a_loop_that_stops_at_the_first_match_is_any() {
    let source = "def f(ps: list[P]) -> bool:\n    mut found = False\n    for p in ps:\n        if p.health <= 0:\n            found = True\n            break\n    found\n";
    assert_eq!(rules(source), ["manual-builtin"]);
    assert_eq!(
        fixed(source),
        "def f(ps: list[P]) -> bool:\n    mut found = any(p.health <= 0 for p in ps)\n    found\n"
    );
    let every = "def f(ps: list[P]) -> bool:\n    mut found = False\n    for p in ps:\n        if check(p):\n            found = True\n    found\n";
    assert_eq!(rules(every), NONE, "without `break` it checks every item, and `any` stops at the first");
    let read = "def f(ps: list[P]) -> bool:\n    mut found = False\n    for p in ps:\n        if found or p.dead:\n            found = True\n            break\n    found\n";
    assert_eq!(rules(read), NONE, "the loop reads the flag");
}

#[test]
fn a_loop_that_keeps_the_largest_is_max() {
    let source = "def f(xs: list[int]) -> int:\n    mut best = xs[0]\n    for x in xs[1:]:\n        if x > best:\n            best = x\n    best\n";
    assert_eq!(rules(source), ["manual-builtin"]);
    assert_eq!(fixed(source), "def f(xs: list[int]) -> int:\n    mut best = max(xs)\n    best\n");
    let smallest = "def f(words: list[str]) -> str:\n    mut first = words[0]\n    for word in words:\n        if first > word:\n            first = word\n    first\n";
    assert_eq!(fixed(smallest), "def f(words: list[str]) -> str:\n    mut first = min(words)\n    first\n");
    let floats = source.replace("list[int]", "list[float]").replace("-> int", "-> float");
    assert_eq!(rules(&floats), NONE, "`max` and `>` order NaN differently");
    let elsewhere = "def f(xs: list[int], ys: list[int]) -> int:\n    mut best = xs[0]\n    for y in ys:\n        if y > best:\n            best = y\n    best\n";
    assert_eq!(rules(elsewhere), NONE, "it starts from another list");
}

#[test]
fn a_counted_while_is_a_for() {
    let source = "def f(n: int):\n    mut i = 0\n    while i < n:\n        g(i)\n        i += 1\n";
    assert_eq!(rules(source), ["counted-while"]);
    assert_eq!(fixed_tidy(source), "def f(n: int):\n    for i in range(n):\n        g(i)\n");
    for (why, source) in [
        (
            "it skips a step",
            "def f(n: int):\n    mut i = 0\n    while i < n:\n        if i == 2:\n            continue\n        i += 1\n",
        ),
        (
            "it's used after",
            "def f(n: int) -> int:\n    mut i = 0\n    while i < n:\n        i += 1\n    i\n",
        ),
        (
            "the limit moves",
            "def f(ref n: int):\n    mut i = 0\n    while i < n:\n        n -= 1\n        i += 1\n",
        ),
        (
            "it moves elsewhere too",
            "def f(n: int):\n    mut i = 0\n    while i < n:\n        i += 2\n        i += 1\n",
        ),
    ] {
        assert_eq!(rules(source), NONE, "{why}");
    }
}

#[test]
fn an_index_that_only_indexes_is_an_item() {
    let source = "def f(enemies: list[E]):\n    for i in range(enemies.len()):\n        draw(enemies[i])\n";
    assert_eq!(rules(source), ["range-len"]);
    assert_eq!(fixed(source), "def f(enemies: list[E]):\n    for enemy in enemies:\n        draw(enemy)\n");
    for (why, source) in [
        ("it's used for more", "def f(xs: list[int]):\n    for i in range(xs.len()):\n        g(i, xs[i])\n"),
        (
            "it writes the list",
            "def f(ref xs: list[int]):\n    for i in range(xs.len()):\n        xs[i] = xs[i] + 1\n",
        ),
        (
            "a dict's keys aren't its items",
            "def f(xs: dict[int, int]):\n    for i in range(xs.len()):\n        g(xs[i])\n",
        ),
        (
            "the name is taken",
            "def f(points: list[int]):\n    point = 1\n    for i in range(points.len()):\n        g(points[i], point)\n",
        ),
    ] {
        assert_eq!(rules(source), NONE, "{why}");
    }
}

#[test]
fn each_kind_of_name_is_written_its_own_way() {
    assert_eq!(name_rules("def DoThing():\n    pass\n"), ["naming-case"]);
    assert_eq!(name_rules("struct my_point:\n    x: int\n"), ["naming-case"]);
    assert_eq!(name_rules("limit = 3\n"), ["naming-case"]);
    assert_eq!(
        name_rules("LIMIT = 3\n\ndef count_it(start: int) -> int:\n    total_now = start\n    total_now\n"),
        NONE
    );
    assert_eq!(
        name_rules(
            "struct Point:\n    _secret: int\n\n    def +(self, other: Point) -> Point:\n        other\n"
        ),
        NONE,
        "a private field and an operator"
    );
}

#[test]
fn a_short_name_is_flagged_unless_it_says_enough() {
    assert_eq!(name_rules("def area(w: int) -> int:\n    w\n"), ["short-name"]);
    assert_eq!(
        name_rules("def f(hp: int) -> int:\n    hp\n").iter().filter(|rule| **rule == "short-name").count(),
        2,
        "and `f`"
    );
    for fine in [
        "def area(x: int, y: int, n: int) -> int:\n    x + y + n\n",
        "def step(dt: float, at: int) -> float:\n    dt\n",
        "def read(fs: Fs) -> int:\n    1\n",
        "def total(items: list[int]) -> int:\n    sum(item for item in items)\n",
    ] {
        assert_eq!(name_rules(fine), NONE, "{fine}");
    }
    assert_eq!(
        name_rules("from std.json import loads as j\n\ndef read() -> int:\n    j(\"1\")\n"),
        NONE,
        "kept apart, see below"
    );
}

#[test]
fn a_local_named_like_a_builtin_is_flagged() {
    assert_eq!(
        name_rules("def count_them(items: list[int]) -> int:\n    sum = 0\n    sum\n"),
        ["shadowed-builtin"]
    );
    assert_eq!(name_rules("def count_them(range: int) -> int:\n    range\n"), ["shadowed-builtin"]);
}

#[test]
fn an_alias_that_changes_nothing_goes() {
    let source = "from std import json as json\n\ndef read() -> str:\n    json.dumps([1])\n";
    assert_eq!(rules(source), ["needless-alias"]);
    assert_eq!(fixed(source), "from std import json\n\ndef read() -> str:\n    json.dumps([1])\n");
    let module = "import std.json as json\n\ndef read() -> str:\n    json.dumps([1])\n";
    assert_eq!(fixed(module), "from std import json\n\ndef read() -> str:\n    json.dumps([1])\n");
    let twice = "from std.math import sqrt, sqrt as root\n\ndef side() -> float:\n    sqrt(root(2.0))\n";
    assert_eq!(rules(twice), ["needless-alias"]);
}

#[test]
fn the_file_put_beside_each_tests_parses_and_has_lints_of_its_own() {
    let mut sources = Sources::new();
    let base = sources.add("beside.fen", BESIDE);
    let module = parse_at(BESIDE, base).map_err(|problem| problem.message);
    let rules: Vec<&str> = module
        .map(|module| lint(&module, &sources).iter().map(|lint| lint.rule).collect())
        .unwrap_or_default();
    assert!(rules.contains(&"unused-name"), "{rules:?}");
}
