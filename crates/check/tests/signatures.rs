//! The signature table: parameter types, inferred return types, and functions that call
//! themselves.

use fennec_check::{ParamType, Type};

use crate::{check, helps, list, named, optional, problems, result, returns};

#[test]
fn return_types_come_from_the_last_expression_and_returns() {
    let cases = [
        ("def f(n: int):\n    n * 2\n", Type::Int),
        ("def f(n: int):\n    n / 2\n", Type::Float),
        ("def f(n: int):\n    f'{n}'\n", Type::Str),
        ("def f(n: int):\n    n > 1\n", Type::Bool),
        ("def f(n: int):\n    x = n\n", Type::Unit),
        ("def f(n: int):\n    pass\n", Type::Unit),
        ("def f(n: int):\n    [n]\n", list(Type::Int)),
        ("def f(n: int):\n    []\n", list(Type::Unknown)),
        ("def f(n: int):\n    (n, 'a')\n", Type::Tuple(vec![Type::Int, Type::Str])),
        ("def f(n: int):\n    {n: 'a'}\n", Type::Dict(Box::new(Type::Int), Box::new(Type::Str))),
        ("def f(n: int):\n    {n}\n", Type::Set(Box::new(Type::Int))),
        ("def f(n: int):\n    if n > 0:\n        return n\n    0\n", Type::Int),
        ("def f(n: int):\n    if n > 0:\n        return n\n    None\n", optional(Type::Int)),
        ("def f(n: int):\n    if n > 0:\n        return None\n    n\n", optional(Type::Int)),
        ("def f(n: int):\n    if n > 0:\n        return n\n    'a'\n", Type::Unknown),
        ("def f(n: int):\n    if n > 0:\n        'a'\n    else:\n        'b'\n", Type::Str),
        ("def f(n: int):\n    if n > 0:\n        'a'\n", Type::Unit),
        ("def f(n: int):\n    while True:\n        return n\n", Type::Int),
        ("def f(n: int):\n    while True:\n        if n > 0:\n            break\n    'done'\n", Type::Str),
        ("def f(n: int):\n    for i in [n]:\n        yield i\n", list(Type::Int)),
        ("def f(n: float) -> float:\n    n * 1.0\n", Type::Float),
        ("def f(n: int):\n    [n] + [n]\n", list(Type::Int)),
        ("def f(n: int):\n    'x' if n > 0 else None\n", optional(Type::Str)),
        ("def f(n: int):\n    x = [1].index(n)\n    x else 0\n", Type::Int),
        ("def f(n: int):\n    [[n], [n]]\n", list(list(Type::Int))),
        (
            "def f(n: int):\n    [(n, 'a'), (n, None)]\n",
            list(Type::Tuple(vec![Type::Int, optional(Type::Str)])),
        ),
        ("def f(n: int):\n    [{n}, {n}]\n", list(Type::Set(Box::new(Type::Int)))),
        (
            "def f(n: int):\n    [{n: n}, {n: n}]\n",
            list(Type::Dict(Box::new(Type::Int), Box::new(Type::Int))),
        ),
        ("def f(n: int):\n    [(n, n), (n,)]\n", list(Type::Unknown)),
        ("def f(n: int):\n    [None, None]\n", list(Type::None)),
        ("def f(n: int):\n    [[1].index(n), [1].index(n), None]\n", list(optional(Type::Int))),
        ("def f(n: int):\n    match n:\n        case 1: return 'a'\n        case _: 'b'\n", Type::Str),
    ];
    for (program, expected) in cases {
        assert_eq!(returns(program, "f"), Ok(expected), "{program}");
    }
}

#[test]
fn functions_that_can_fail_return_results() {
    let program = "
        error Oops:
            why: str

        error Other:
            why: str

        def a(n: int):
            if n > 1:
                fail Oops('big')
            n

        def b(n: int):
            x = a(n)?
            if x > 2:
                fail Other('x')
            x

        def c(n: int):
            a(n)? + 1

        def d(n: int):
            a(n)

        def e(n: int) -> int:
            a(n)?

        def g(n: int):
            fail Oops('always')
    ";
    let oops = named("Oops");
    assert_eq!(returns(program, "a"), Ok(result(Type::Int, oops.clone())));
    assert_eq!(returns(program, "b"), Ok(result(Type::Int, Type::Unknown)));
    assert_eq!(returns(program, "c"), Ok(result(Type::Int, oops.clone())));
    assert_eq!(returns(program, "d"), Ok(result(Type::Int, oops.clone())));
    assert_eq!(returns(program, "e"), Ok(result(Type::Int, oops.clone())));
    assert_eq!(returns(program, "g"), Ok(result(Type::Unknown, oops)));
    let signatures = check(program).unwrap();
    assert!(signatures.get("a").unwrap().can_fail);
    assert!(!signatures.get("d").unwrap().can_fail);
}

#[test]
fn signatures_cover_methods_nested_functions_and_the_standard_library() {
    let program = "
        struct Enemy:
            health = 100

            def take_damage(mut self, amount: int):
                self.health -= amount

            def at(health: int):
                Enemy(health=health)

        def outer(start: int):
            def inner(n: int):
                n + start
            inner(1)

        def countdown(n: int):
            yield n
    ";
    let signatures = check(program).unwrap();
    let take_damage = signatures.get("Enemy.take_damage").unwrap();
    assert_eq!(
        take_damage.params,
        [
            ParamType { name: "self".to_owned(), ty: named("Enemy"), is_mut: true },
            ParamType { name: "amount".to_owned(), ty: Type::Int, is_mut: false },
        ]
    );
    assert_eq!(take_damage.returns, Type::Unit);
    assert_eq!(signatures.get("Enemy.at").unwrap().returns, named("Enemy"));
    assert_eq!(signatures.get("outer.inner").unwrap().returns, Type::Int);
    assert_eq!(signatures.get("outer").unwrap().returns, Type::Int);
    assert!(signatures.get("countdown").unwrap().generator);
    assert_eq!(signatures.get("Vec3.length").unwrap().returns, Type::Float);
    assert_eq!(signatures.get("Vec3.zero").unwrap().returns, named("Vec3"));
    assert!(signatures.get("missing").is_none());
    let names: Vec<&str> = signatures.iter().map(|(name, _)| name).collect();
    assert!(names.contains(&"outer.inner") && names.contains(&"Vec3.+"), "{names:?}");
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
}

#[test]
fn written_types_resolve() {
    let program = "
        struct Thing:
            n = 1

        type Things = list[Thing]
        type Loop = Loop

        def f(a: int?, b: list[str], c: set[int], d: dict[str, int], e: (int, str), g: Id[Thing], h: IdMap[Thing], i: Path, j: Things, k: T, l: Loop, m: SoaList[Thing], n: None, o: bool, p: float, q: Fs, r: list):
            pass
    ";
    let signatures = check(program).unwrap();
    let types: Vec<Type> = signatures.get("f").unwrap().params.iter().map(|param| param.ty.clone()).collect();
    assert_eq!(
        types,
        [
            optional(Type::Int),
            list(Type::Str),
            Type::Set(Box::new(Type::Int)),
            Type::Dict(Box::new(Type::Str), Box::new(Type::Int)),
            Type::Tuple(vec![Type::Int, Type::Str]),
            Type::Id(Box::new(named("Thing"))),
            Type::IdMap(Box::new(named("Thing"))),
            named("Path"),
            list(named("Thing")),
            Type::Unknown,
            Type::Unknown,
            list(named("Thing")),
            Type::None,
            Type::Bool,
            Type::Float,
            named("Fs"),
            list(Type::Unknown),
        ]
    );
}

#[test]
fn field_types_come_from_their_types_or_defaults() {
    let program = "
        struct Config:
            volume = 0.8
            fullscreen = False
            levels = ['intro']
            weapon = None
            later = Config.make
            count: int = 'wrong'

            def make():
                Config()

        def main(stdout: Stdout):
            c = Config()
            stdout.print(c.volume + 1, c.fullscreen + 1, c.levels[0] + 1, c.weapon + 1, c.later.volume + 1.0)
    ";
    assert_eq!(
        problems(program),
        [
            "`count` should be an int, but this is a str @ 'wrong'".replace('\'', "\"").as_str(),
            "can't mix int and float in `+` @ c.volume + 1",
            "can't use `+` on a bool and an int @ c.fullscreen + 1",
            "can't use `+` on a str and an int @ c.levels[0] + 1",
        ]
    );
}

#[test]
fn recursive_functions_write_their_return_type() {
    let program = "
        def fib(n: int):
            if n < 2:
                return n
            fib(n - 1) + fib(n - 2)

        def fact(n: int) -> int:
            1 if n <= 1 else n * fact(n - 1)

        def ping(n: int):
            pong(n)

        def pong(n: int):
            if n > 0:
                return ping(n - 1)
            'done'

        def walk(mut items: list[int], label: str?):
            walk(mut items, label)

        def outer():
            def inner(n: int):
                inner(n)
            inner(1)
    ";
    assert_eq!(
        problems(program),
        [
            "`fib` calls itself, so its return type has to be written @ def fib(n: int)",
            "`ping` calls itself, so its return type has to be written @ def ping(n: int)",
            "`walk` calls itself, so its return type has to be written @ def walk(mut items: list[int], label: str?)",
            "`inner` calls itself, so its return type has to be written @ def inner(n: int)",
        ]
    );
    assert_eq!(
        helps(program),
        [
            "write it after the parameters: `def fib(n: int) -> int:`",
            "write the type it returns after the parameters: `def ping(n: int) -> <type>:`",
            "write the type it returns after the parameters: `def walk(mut items: list[int], label: str?) -> <type>:`",
            "write the type it returns after the parameters: `def inner(n: int) -> <type>:`",
        ]
    );
    assert_eq!(returns(program, "fact"), Err(problems(program)));
}

#[test]
fn written_return_types_are_checked() {
    let program = "
        def a(n: int) -> str:
            if n > 0:
                return n
            'ok'

        def b(n: int) -> int:
            [1].index(n)

        def c(n: int) -> int?:
            if n > 0:
                return None
            n

        def d(n: int) -> int:
            if n > 0:
                return n
            while True:
                pass

        def e(n: int) -> int:
            for i in [n]:
                yield 'x'
    ";
    assert_eq!(
        problems(program),
        ["`a` returns a str, but this is an int @ n", "this might be None @ [1].index(n)",]
    );
    assert_eq!(helps("def a(n: int) -> str:\n    n\n"), ["return a str, or change the return type"]);
}

#[test]
fn a_clean_program_has_no_problems() {
    let program = "
        struct Enemy:
            health = 100

        def main(stdout: Stdout):
            stdout.print(Enemy().health)
    ";
    assert!(check(program).is_ok());
}

#[test]
fn values_from_several_places_combine() {
    let cases = [
        ("def f(n: int):\n    [[n], [None]]\n", list(list(optional(Type::Int)))),
        ("def f(n: int):\n    [{n}, {None}]\n", list(Type::Set(Box::new(optional(Type::Int))))),
        (
            "def f(n: int):\n    [{n: n}, {None: n}]\n",
            list(Type::Dict(Box::new(optional(Type::Int)), Box::new(Type::Int))),
        ),
        ("def f(n: int):\n    [[n], 'a']\n", list(Type::Unknown)),
    ];
    for (program, expected) in cases {
        assert_eq!(returns(program, "f"), Ok(expected), "{program}");
    }
    let program = "
        error Oops:
            why: str

        error Other:
            why: str

        def a(n: int):
            if n > 1:
                fail Oops('x')
            n

        def b(n: int):
            if n > 1:
                fail Other('x')
            [1].index(n)

        def c(n: int):
            if n > 0:
                return a(n)
            b(n)

        def d(n: int) -> int:
            if n > 1:
                fail Oops('x')
            a(n)
    ";
    assert_eq!(returns(program, "c"), Ok(result(optional(Type::Int), Type::Unknown)));
    assert_eq!(returns(program, "d"), Ok(result(Type::Int, named("Oops"))));
}

#[test]
fn failing_inside_a_match_or_with_context_counts() {
    let program = "
        error Oops:
            why: str

        error Unreadable:
            path: str

        def a(n: int):
            match n:
                case 1: fail Oops('one')
                case _: n

        def b(fs: Fs, path: str):
            fs.read_text(path)? from Unreadable(path)
    ";
    assert_eq!(returns(program, "a"), Ok(result(Type::Int, named("Oops"))));
    assert_eq!(returns(program, "b"), Ok(result(Type::Str, named("Unreadable"))));
}

#[test]
fn loops_that_break_out_of_blocks_still_end() {
    let cases = [
        "def f(n: int):\n    while True:\n        with n as held:\n            break\n    'done'\n",
        "def f(n: int):\n    while True:\n        match n:\n            case _: break\n    'done'\n",
    ];
    for program in cases {
        assert_eq!(returns(program, "f"), Ok(Type::Str), "{program}");
    }
}

#[test]
fn only_the_value_a_function_ends_with_is_checked_against_its_return_type() {
    let program = "
        error Oops:
            why: str

        def risky(n: int):
            if n > 1:
                fail Oops('x')
            n

        def a() -> int:
            return 1
            'never reached'

        def b(n: int) -> str:
            yield n
            return n

        def c(n: int) -> int:
            risky(n)

        def d(n: int) -> int:
            x = risky(n)?
            risky(x)
    ";
    assert_eq!(problems(program), ["this is the result of something that can fail @ risky(n)"]);
}
