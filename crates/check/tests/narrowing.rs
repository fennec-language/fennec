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
            player.name

        def b(names: list[str]):
            for name in names:
                player = find(name)
                if player is None:
                    continue
                player.name
            for name in names:
                player = find(name)
                if player is None:
                    break
                player.name

        error Missing:
            name: str

        def c(name: str):
            player = find(name)
            if not (player is not None):
                fail Missing(name)
            player.name

        def d(name: str):
            player = find(name)
            if player is None:
                stdout_less = 1
            player.name
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
            player.name
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
            player.name.upper()
            find(name).name.upper()

        def e(name: str):
            player = find(name)?
            player.name

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
            player.name

        def b(names: list[str]):
            mut best = find('')
            for name in names:
                if best is None or name.len() > best.name.len():
                    best = Player(name=name)
            best.name

        def c(names: list[str]):
            mut last = find('a')
            last = Player(name='y')
            last.name
            last = None
            last.name
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
                case Red: 2
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
            match risky(n):
                case Ok(value): value + 'a'
                case Err(error): error + 1

        def b(shape: Shape, other: T):
            match shape:
                case Circle(r): r + 1
            match other:
                case .Circle(dotted): dotted + 1
                case Circle(plain): plain + 1
                case _: 0
            match 3:
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
            "can't use `+` on an int and a str @ value + \"a\"",
            "can't use `+` on an Oops and an int @ error + 1",
            "`T` isn't a type @ T",
            "can't mix int and float in `+` @ r + 1",
            "can't mix int and float in `+` @ dotted + 1",
            "this pattern has 3 parts, but the value has 2, so it can't match @ case (a, b, c): c\n",
        ]
    );
}
