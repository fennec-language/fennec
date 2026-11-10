//! Optionals: a `T?` can't be used as a `T` until it's known not to be None, by an `is None`
//! check, `?`, `?.` or `else`. `match` over an enum covers every case.

use crate::{helps, problems};

const FIND: &str = "
struct Weapon:
    damage: int

struct Player:
    name: str
    weapon: Weapon? = None

def find(name: str) -> Player?:
    if name == '':
        return None
    Player(name=name)
";

fn with_find(body: &str) -> Vec<String> {
    crate::in_main_after(FIND, body)
}

#[test]
fn optionals_are_checked_before_use() {
    let program = "
        player = find('a')
        stdout.print(player.name, player.weapon, find('b').name.upper(), player + 1, -player)
        for n in find('c'):
            pass
        if player:
            pass
        nothing = None
        stdout.print(nothing.name, nothing[0])
    ";
    assert_eq!(
        with_find(program),
        [
            "this might be None @ player.name",
            "this might be None @ player.weapon",
            "this might be None @ find(\"b\").name",
            "this might be None @ player",
            "this might be None @ player",
            "this might be None @ find('c')".replace('\'', "\"").as_str(),
            "`if` needs a bool, but this is a Player? @ player",
            "this might be None @ nothing.name",
            "this might be None @ nothing[0]",
        ]
    );
    let help = helps(&format!("{FIND}\ndef f():\n    find('a').name\n"));
    assert_eq!(
        help,
        [
            "end the chain with `else` and a default, like `x.name else \"\"`, or check with `if x is None:` first"
        ]
    );
}

#[test]
fn leaving_early_when_none_narrows_the_rest() {
    let program = "
        def a(name: str):
            player = find(name)
            if player is None:
                return
            _ = player.name

        def b(names: list[str]):
            for name in names:
                player = find(name)
                if player is None:
                    continue
                _ = player.name
            for name in names:
                player = find(name)
                if player is None:
                    break
                _ = player.name

        error Missing:
            name: str

        def c(name: str):
            player = find(name)
            if not (player is not None):
                fail Missing(name)
            _ = player.name

        def d(name: str):
            player = find(name)
            if player is None:
                stdout_less = 1
            _ = player.name
    ";
    assert_eq!(
        problems(&format!("{FIND}\n{}", crate::dedent(program))),
        ["this might be None @ player.name"]
    );
}

#[test]
fn checks_narrow_the_code_they_guard() {
    let program = "
        def a(name: str):
            player = find(name)
            if player is not None:
                stdout_less = player.name
            if player is not None and player.weapon is not None:
                stdout_less = player.weapon.damage
            if player is None or player.name == '':
                stdout_less = 1
            label = player.name if player is not None else '?'
            other = '?' if player is None else player.name
            while player is not None:
                stdout_less = player.name
            if player is None:
                pass
            elif player.name == '':
                pass
            else:
                stdout_less = player.name
            names = [p.name for p in [player] if p is not None]
            _ = player.name
    ";
    assert_eq!(
        problems(&format!("{FIND}\n{}", crate::dedent(program))),
        ["this might be None @ player.name"]
    );
}

#[test]
fn narrowing_ends_with_the_code_it_guards() {
    let program = "
        def a(name: str):
            player = find(name)
            if player is not None:
                pass
            stdout_less = player.name
            ok = player is not None and True
            again = player.name
            either = player.name if True else ''
    ";
    assert_eq!(
        problems(&format!("{FIND}\n{}", crate::dedent(program))),
        [
            "this might be None @ player.name",
            "this might be None @ player.name",
            "this might be None @ player.name",
        ]
    );
}

#[test]
fn question_mark_and_else_chains() {
    let program = "
        def b(name: str):
            damage = find(name).weapon.damage else 0
            damage + 1

        def c(name: str):
            name_length = (find(name).name else '').len()

        def d(name: str):
            player = find(name) else Player(name='x')
            _ = player.name.upper()
            find(name).name.upper()

        def e(name: str):
            player = find(name)?
            _ = player.name

        def f(name: str):
            longer = find(name).name.len() + 1 else 0
    ";
    assert_eq!(
        problems(&format!("{FIND}\n{}", crate::dedent(program))),
        [
            "this might be None @ find(name).name",
            "`?` needs a result, but this is a Player? @ find(name)",
            "this might be None @ find(name).name",
        ]
    );
}

#[test]
fn assigning_a_value_counts_as_checking() {
    let program = "
        def a(name: str):
            mut player = find(name)
            if player is None:
                player = Player(name='x')
            _ = player.name

        def b(names: list[str]):
            mut best = find('')
            for name in names:
                if best is None or name.len() > best.name.len():
                    best = Player(name=name)
            best.name

        def c(names: list[str]):
            mut last = find('a')
            last = Player(name='y')
            _ = last.name
            last = None
            _ = last.name
    ";
    assert_eq!(
        problems(&format!("{FIND}\n{}", crate::dedent(program))),
        ["this might be None @ best.name", "this might be None @ last.name",]
    );
}

#[test]
fn match_over_an_enum_covers_every_case() {
    let program = "
        enum Light:
            Red
            Green
            Blue(level: int)

        def a(light: Light):
            match light:
                case .Red: 1
                case .Green: 2
                case .Blue(level): level

        def b(light: Light):
            match light:
                case .Red: 1

        def c(light: Light):
            match light:
                case .Blue(3): 1
                case .Red: 2
                case _: 3

        def d(light: Light):
            match light:
                case other: 1

        def e(light: Light):
            match light:
                case .Blue(3): 1
                case .Red: 2
                case .Green: 3

        def f(light: Light):
            match light:
                case .Purple: 1
                case .Red: 2
                case _: 3
    ";
    assert_eq!(
        problems(program),
        [
            "this `match` doesn't handle `Light.Green` or `Light.Blue` @ light",
            "this `match` doesn't handle `Light.Blue` @ light",
            "`Light` has no case called `Purple` @ case .Purple: 1\n",
        ]
    );
    let help = helps("enum L:\n    A\n    B\n    C\n\ndef f(l: L):\n    match l:\n        case .A: 1\n");
    assert_eq!(help, ["add `case .B:` and `case .C:`, or `case _:` for everything else"]);
}

#[test]
fn match_binds_the_fields_of_each_case() {
    let program = "
        enum Shape:
            Circle(radius: float)
            Pair(sides: (int, str))

        error Missing:
            name: str

        def area(shape: Shape):
            match shape:
                case .Circle(radius): radius + 1.0
                case .Pair((count, label)): label + count

        def name(shape: Shape):
            match shape:
                case .Circle(radius, extra): 1
                case _: 2

        def other(n: int):
            match n:
                case 1: 'one'
                case (a, b): a
                case Circle(r): r + 1
                case Missing(name): name + 1
                case value: value + 1.5
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on a str and an int @ label + count",
            "this pattern has 2 parts, but the value has 1 fields @ case .Circle(radius, extra): 1\n",
            "this pattern is a tuple, but the value is an int @ case (a, b): a\n",
            "can't use `+` on a str and an int @ name + 1",
            "can't mix int and float in `+` @ value + 1.5",
        ]
    );
}

/// A pattern can name the fields it matches, in any order and leaving any out: each name gets
/// its field's type. Another kit's type is matched that way only, so a field added to it with a
/// default never breaks a `match`. A kit's own types still match by position.
#[test]
fn a_pattern_names_the_fields_it_matches() {
    let fields = |source: &str| -> Vec<String> {
        problems(source)
            .iter()
            .map(|problem| problem.split(" @ ").next().unwrap_or_default().to_owned())
            .collect()
    };
    let program = "
struct Point:
    x: int
    y: str

enum Shape:
    Circle(radius: float)
    Rect(width: float, height: int)

def area(shape: Shape) -> float:
    match shape:
        case .Rect(height=h, width=w): w * h
        case .Circle(radius=r): r * r

def label(point: Point) -> str:
    match point:
        case Point(y=name): name
        case Point(x, y): y

def read(fs: Fs) -> str:
    text = fs.read_text(\"a\") else as why:
        match why:
            case IoError(reason=why):
                return why
    text
";
    assert_eq!(fields(program), ["can't mix int and float in `*`"]);
    let unknown = "
struct Point:
    x: int

def f(point: Point) -> int:
    match point:
        case Point(z=a): a
";
    assert_eq!(fields(unknown), ["`Point` has no field `z`: it has `x`"]);
    let positional = "
def read(fs: Fs) -> str:
    text = fs.read_text(\"a\") else as why:
        match why:
            case IoError(why):
                return why
    text
";
    assert_eq!(
        fields(positional),
        ["`IoError` comes from another kit, so this pattern names the fields it reads"]
    );
    assert_eq!(
        helps(positional),
        [
            "write `IoError(reason=...)`, or `IoError()` to read none, and a field added to it never breaks this; `fennec fix` adds the names"
        ]
    );
    let none_read = "
def read(fs: Fs) -> str:
    text = fs.read_text(\"a\") else as why:
        match why:
            case IoError():
                return \"failed\"
    text
";
    assert_eq!(fields(none_read), Vec::<String>::new());
}

#[test]
fn match_arms_make_a_value() {
    let program = "
        enum Light:
            Red
            Green

        def label(light: Light):
            text = match light:
                case .Red: 'stop'
                case .Green:
                    if True:
                        'go'
                    else:
                        'maybe'
            text + 1
    ";
    assert_eq!(problems(program), ["can't use `+` on a str and an int @ text + 1"]);
}

#[test]
fn or_conditions_narrow_when_they_are_false() {
    let program = "
        def a(x: str, y: str):
            first = find(x)
            second = find(y)
            if first is None or second is None:
                first.name
                return
            first.name + second.name
    ";
    assert_eq!(problems(&format!("{FIND}\n{}", crate::dedent(program))), ["this might be None @ first.name"]);
}

#[test]
fn checked_fields_are_forgotten_when_their_value_changes() {
    let program = "
        def a(name: str):
            mut player = Player(name=name)
            if player.weapon is not None:
                stdout_less = player.weapon.damage
                player = Player(name='other')
                again = player.weapon.damage
            if player.weapon is not None:
                player.weapon = None
                more = player.weapon.damage

        def b(ghost_free: int):
            if ghost is None:
                pass
    ";
    assert_eq!(
        problems(&format!("{FIND}\n{}", crate::dedent(program))),
        [
            "this might be None @ player.weapon.damage",
            "this might be None @ player.weapon.damage",
            "`ghost` doesn't exist @ ghost",
        ]
    );
}

#[test]
fn match_cases_bind_typed_values() {
    let program = "
        enum Shape:
            Circle(radius: float)

        error Oops:
            why: str

        def risky(n: int):
            if n > 1:
                fail Oops('x')
            n

        def a(n: int):
            value = risky(n) else as error:
                error + 1
            value + 'a'

        def b(shape: Shape, other: T):
            match shape:
                case .Circle(r): r + 1
            _ = match other:
                case .Circle(dotted): dotted + 1
                case Circle(plain): plain + 1
                case _: 0
            _ = match 3:
                case .Circle(number): number + 1
                case _: 0
            pair = (1, 'a')
            match pair:
                case (a, b, c): c
                case _: 0
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on an Oops and an int @ error + 1",
            "can't use `+` on an int and a str @ value + \"a\"",
            "`T` isn't a type @ T",
            "can't mix int and float in `+` @ r + 1",
            "can't mix int and float in `+` @ dotted + 1",
            "this pattern has 3 parts, but the value has 2, so it can't match @ case (a, b, c): c\n",
        ]
    );
}

#[test]
fn a_list_written_where_a_list_of_optionals_goes_is_one() {
    let program = "
        def f() -> int:
            mut numbers: list[int?] = [1, 2]
            numbers.append(None)
            numbers[0] + 1
    ";
    assert_eq!(problems(program).len(), 1, "{:?}", problems(program));
    assert!(problems(program)[0].contains("numbers[0]"), "{:?}", problems(program));
}

#[test]
fn what_a_mut_parameter_can_make_none_is_checked_again() {
    let program = "
        def cleared(ref value: int?):
            value = None

        def f(n: int?) -> int:
            mut held = n
            if held is not None:
                cleared(ref held)
                return held + 1
            0
    ";
    assert_eq!(problems(program), ["this might be None @ held"]);
    let keeps_what_it_knows = "
        def bumped(ref value: int):
            value += 1

        def f(n: int?) -> int:
            mut held = n
            if held is not None:
                bumped(ref held)
                return held + 1
            0
    ";
    assert_eq!(problems(keeps_what_it_knows), Vec::<String>::new());
}

#[test]
fn a_loop_sees_what_its_body_changes() {
    let cases = [
        ("mut x: int? = 5\nfor n in names:\n    use(x + 1)\n    x = None\n", "x"),
        ("mut x: int? = 5\nmut more = True\nwhile x + 1 > 0 and more:\n    x = None\n", "x"),
        ("mut x: int? = 5\nfor n in names:\n    for m in names:\n        use(x + 1)\n    x = None\n", "x"),
        ("mut x: int? = 5\nfor n in names:\n    for m in names:\n        x = None\n    use(x + 1)\n", "x"),
        ("mut x: int? = 5\nfor n in names:\n    use(x + 1)\n    if n == 'a':\n        x = None\n", "x"),
        ("mut x: int? = 5\nfor n in names:\n    use(x + 1)\n    x = None\n    continue\n", "x"),
        ("mut x: int? = 5\nfor n in names:\n    use(x + 1)\n    x = None\n    break\n", "x"),
        (
            "mut x: int? = 5\nfor n in names:\n    x = None\n    if n == 'a':\n        break\n    x = 1\nuse(x + 1)\n",
            "x",
        ),
        (
            "mut x: int? = 5\nfor n in names:\n    match n:\n        case 'a':\n            use(x + 1)\n        case _:\n            x = None\n",
            "x",
        ),
        ("mut x: int? = 5\nfor n in names:\n    use(x + 1)\n    clear(ref x)\n", "x"),
        // It never holds None, but the loop's first look at it doesn't know that.
        ("mut x: int? = 5\nfor n in names:\n    x = x + 1\n", "x"),
        (
            "mut h = Held(x=1)\nif h.x is not None:\n    for n in names:\n        use(h.x + 1)\n        h.x = None\n",
            "h.x",
        ),
        (
            "mut h = Held(x=1)\nif h.x is not None:\n    for n in names:\n        use(h.x + 1)\n        h.clear()\n",
            "h.x",
        ),
    ];
    for (body, read) in cases {
        let found = problems(&in_a_loop_function(body));
        assert_eq!(found, [format!("this might be None @ {read}")], "{body}");
    }
}

#[test]
fn a_loop_keeps_what_its_body_leaves_alone() {
    let cases = [
        "x: int? = 5\nif x is None:\n    return\nfor n in names:\n    use(x + 1)\n",
        "mut x: int? = 5\nif x is None:\n    return\nfor n in names:\n    use(x + 1)\nx = None\nuse(x else 0)\n",
        "mut x: int? = None\nfor n in names:\n    x = 1\n    use(x + 1)\n",
        "mut x: int? = None\nfor n in names:\n    if x is not None:\n        use(x + 1)\n    x = 1\n",
        "h = Held(x=1)\nif h.x is not None:\n    for n in names:\n        use(h.x + 1)\n        _ = h.show()\n",
    ];
    for body in cases {
        assert_eq!(problems(&in_a_loop_function(body)), Vec::<String>::new(), "{body}");
    }
}

fn in_a_loop_function(body: &str) -> String {
    let body = format!("    {}", body.trim_end().replace('\n', "\n    "));
    format!(
        "struct Held:
    x: int?

    def clear(ref self):
        self.x = None

    def show(self) -> int:
        1

def use(n: int):
    pass

def clear(ref slot: int?):
    slot = None

def run(names: list[str]):
{body}
"
    )
}

#[test]
fn an_else_chain_stops_at_a_failing_step_too() {
    let empty: [&str; 0] = [];
    let types = "error Lost:\n    why: str\n\nstruct Part:\n    name = \"\"\n\n    def again(self) -> Part fails Lost:\n        self\n\ndef find(n: int) -> Part fails Lost:\n    if n > 1: fail Lost(why=\"big\")\n    Part()\n";
    let kept = |body: &str| crate::in_main_after(types, &format!("{body}\nstdout.print(1)\n"));
    for body in [
        "name: str = find(1).name else \"?\"\nstdout.print(name)",
        "size: int = find(1).name.len() else 0\nstdout.print(size)",
        "kept: str = find(1).expect(\"there\").name\nstdout.print(kept)",
    ] {
        assert_eq!(kept(body), empty, "{body}");
    }
    let second = kept("name: str = find(1).again().name else \"?\"\nstdout.print(name)");
    assert_eq!(second, ["this is the result of something that can fail @ find(1).again().name"]);
    let twice = kept("part: Part = find(1).again() else Part()\nstdout.print(part.name)");
    assert_eq!(
        twice,
        ["two steps of this chain can fail, and `else` stops at one @ find(1).again() else Part()"]
    );
    let said = kept("stdout.print(find(1).name)");
    assert_eq!(said, ["this is the result of something that can fail @ find(1).name"]);
    let bracketed = kept("stdout.print((find(1).name else \"?\") + find(2).name)");
    assert_eq!(bracketed, ["this is the result of something that can fail @ find(2).name"]);
}
