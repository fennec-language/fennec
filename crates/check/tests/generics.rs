//! Generic functions and types, and interfaces. A type parameter stands for one type per
//! use, worked out from the arguments or written in brackets. Its bounds say what that type
//! has to be able to do, and inside the generic code, all a value of it can do. A type fits
//! an interface by having its methods; `Eq`, `Hashable`, `Ordered` and `Format` are built in.

use fennec_check::Type;

use crate::{helps, in_main, problems, returns};

const SHAPES: &str = "
interface Shape:
    def area(self) -> float

struct Circle:
    radius: float

    def area(self):
        3.14 * self.radius * self.radius

struct Square:
    side: float

    def area(self) -> float:
        return self.side * self.side

struct Rock:
    weight = 1

    def area(self) -> int:
        return 1
";

fn with_shapes(program: &str) -> Vec<String> {
    problems(&format!("{SHAPES}\n{}", crate::dedent(program)))
}

#[test]
fn type_parameters_are_chosen_by_each_call() {
    let program = "
        def first[T](items: list[T]) -> T:
            return items[0]

        def pair[A, B](a: A, b: B) -> (A, B):
            return (a, b)

        def main(stdout: Stdout):
            stdout.print(first([1]) + 1, first(['a']).upper(), pair(1, 'a')[1].upper())
            stdout.print(first([1]) + 'a')
    ";
    assert_eq!(problems(program), ["can't use `+` on an int and a str @ first([1]) + \"a\""]);
    let program = "def first[T](items: list[T]) -> T:\n    return items[0]\n";
    assert_eq!(returns(program, "first"), Ok(Type::Param("T".to_owned())));
}

#[test]
fn a_type_parameter_fits_only_one_type_per_call() {
    let program = "
        def same[T](a: T, b: T) -> T:
            return a

        def main(stdout: Stdout):
            stdout.print(same(1, 'a'))
    ";
    assert_eq!(problems(program), ["`b` should be an int, but this is a str @ \"a\""]);
}

#[test]
fn types_can_be_written_in_brackets_when_nothing_shows_them() {
    let program = "
        def make[T]() -> list[T]:
            return []

        def main(stdout: Stdout):
            stdout.print(make())
            numbers: list[int] = make()
            stdout.print(make[str]())
    ";
    assert_eq!(problems(program), ["can't tell what `T` is in this call @ make()"]);
}

#[test]
fn values_of_a_type_parameter_can_do_only_what_its_bounds_say() {
    let program = "
        def total[T: Shape](shapes: list[T]) -> float:
            return sum(shape.area() for shape in shapes)

        def wrong[T: Shape](shape: T):
            shape.perimeter()

        def unbound[T](value: T):
            value.area()

        def compare[T](a: T, b: T) -> bool:
            return a < b or a == b

        def ordered[T: Ordered](a: T, b: T) -> bool:
            return a < b and a == b

        def main(stdout: Stdout):
            stdout.print(total([Circle(radius=1.0)]))
    ";
    assert_eq!(
        with_shapes(program),
        [
            "`T` has no method `perimeter` @ shape.perimeter()",
            "`T` has no method `area` @ value.area()",
            "`T` could be any type, so it might not have an order @ a < b",
            "`T` could be any type, so `==` might not work on it @ a == b",
        ]
    );
    assert_eq!(
        helps(&format!("{SHAPES}\ndef unbound[T](value: T):\n    value.area()\n")),
        ["bind `T` to an interface that has it, like `T: HasLength`"]
    );
}

#[test]
fn an_interface_is_not_a_type() {
    let program = "
        def biggest(shapes: list[Shape]) -> float:
            return 0.0
    ";
    assert_eq!(with_shapes(program), ["`Shape` is an interface, not a type @ Shape"]);
    assert_eq!(
        helps(&format!("{SHAPES}\ndef f(shape: list[Shape]):\n    pass\n")),
        [
            "take a type parameter that fits it, like `def f[T: Shape](items: list[T])`, or use an enum for a mix of types"
        ]
    );
}

#[test]
fn calls_check_their_types_fit_the_bounds() {
    let program = "
        def total[T: Shape](shapes: list[T]) -> float:
            return sum(shape.area() for shape in shapes)

        def largest[T: Ordered & Hashable](items: list[T]) -> T:
            return max(items)

        def main(stdout: Stdout):
            stdout.print(total([Rock()]), total([1]), total([Circle(radius=1.0), Circle(radius=2.0)]))
            stdout.print(largest([1, 2]), largest([[1]]), largest([Circle(radius=1.0)]))
    ";
    assert_eq!(
        with_shapes(program),
        [
            "`Rock` isn't a `Shape`: it has no `area(self) -> float`. Its `area` returns an int @ total([Rock()])",
            "an int isn't a `Shape`: it has no `area(self) -> float` @ total([1])",
            "a list[int] isn't a `Hashable`: a list[int] can't be hashed @ largest([[1]])",
            "`Circle` isn't an `Ordered`: `Circle` can't be ordered @ largest([Circle(radius=1.0)])",
        ]
    );
    let program =
        "def total[T: Shape](shapes: list[T]):\n    0\n\ndef main(stdout: Stdout):\n    total([1])\n";
    assert_eq!(helps(&format!("{SHAPES}\n{program}")), ["give an int the method `def area(self) -> float`"]);
}

#[test]
fn one_call_holds_one_type_for_a_type_parameter() {
    let program = "
        def biggest[T: Shape](shapes: list[T]) -> float:
            return max(shape.area() for shape in shapes)

        def main(stdout: Stdout):
            stdout.print(biggest([Circle(radius=1.0), Square(side=1.0)]))
    ";
    assert_eq!(
        with_shapes(program),
        ["this list mixes a Circle and a Square @ [Circle(radius=1.0), Square(side=1.0)]"]
    );
}

#[test]
fn generic_functions_call_each_other() {
    let program = "
        def area_of[T: Shape](shape: T) -> float:
            return shape.area()

        def total[T: Shape](shapes: list[T]) -> float:
            return sum(area_of(shape) for shape in shapes)

        def loose[T](value: T) -> float:
            return area_of(value)

        def main(stdout: Stdout):
            stdout.print(total([Square(side=1.0)]))
    ";
    assert_eq!(with_shapes(program), ["`T` isn't a `Shape`: `T` could be any type @ area_of(value)"]);
}

#[test]
fn generic_types_fill_in_their_parameters() {
    let program = "
        struct Stack[T]:
            items = list[T]()

            def push(ref self, item: T):
                self.items.append(item)

            def top(self) -> T?:
                if len(self.items) == 0:
                    return None
                return self.items[-1]

        struct Ranking[T: Ordered]:
            best = list[T]()

        def main(stdout: Stdout):
            mut names = Stack[str]()
            names.push('Ash')
            stdout.print(names.top() else '', names.items[0].upper())
            names.push(3)
            mut numbers = Stack()
            numbers.push(1)
            stdout.print(numbers.items[0] + 'a')
            stdout.print(Ranking(best=[1, 2]), Ranking(best=[{1}]))
    ";
    assert_eq!(
        problems(program),
        [
            "`item` should be a str, but this is an int @ 3",
            "can't use `+` on an int and a str @ numbers.items[0] + \"a\"",
            "a set[int] isn't an `Ordered`: a set[int] can't be ordered @ Ranking(best=[{1}])",
        ]
    );
    let program =
        "struct Stack[T]:\n    items = list[T]()\n\ndef main(stdout: Stdout):\n    empty = Stack()\n";
    assert_eq!(problems(program), ["can't tell what `T` is in this call @ Stack()"]);
}

#[test]
fn a_generic_type_built_where_one_is_expected_takes_its_types_from_there() {
    let program = "
        struct Box[T]:
            value: T

        struct Held[T]:
            items: list[Box[T]]
            plain: list[T]

        def show(box: Box[int | str]) -> int:
            return 1

        def count(held: Held[int | str]) -> int:
            return len(held.items)

        def main(stdout: Stdout):
            name = 'x'
            kept: Box[int | str] = Box(value=4)
            stdout.print(show(Box(value=3)), show(Box(value=name)), show(kept))
            stdout.print(count(Held(items=[Box(value=1), Box(value=name)], plain=[name, 2])))
            stdout.print(show(Box(value=1.5)))
    ";
    assert_eq!(problems(program), ["`value` should be an int | str, but this is a float @ 1.5"]);
}

#[test]
fn generic_types_need_their_types_written() {
    let program = "
        struct Stack[T]:
            items = list[T]()

        def f(a: Stack, b: Stack[int, str], c: Stack[int]):
            pass
    ";
    assert_eq!(
        problems(program),
        ["`Stack` needs 1 type in brackets @ Stack", "`Stack` needs 1 type in brackets @ Stack[int, str]"]
    );
}

#[test]
fn function_types_let_arrow_functions_be_passed_to_your_own_functions() {
    let program = "
        def apply(f: (int) -> int, n: int) -> int:
            return f(n)

        def combine(f: (int, int) -> int) -> int:
            return f(1, 2)

        def main(stdout: Stdout):
            stdout.print(apply(n => n * 2, 3) + 1, combine((a, b) => a + b))
            stdout.print(apply(n => 'text', 3), combine(a => a))
    ";
    assert_eq!(
        problems(program),
        [
            "`f` should be a (int) -> int, but this is a (int) -> str @ n => \"text\"",
            "`f` should be a (int, int) -> int, but this is a (int) -> int @ a => a",
        ]
    );
}

#[test]
fn self_in_an_interface_is_the_type_that_fits_it() {
    let program = "
        interface Addable:
            def +(self, other: Self) -> Self

        interface Before:
            def <(self, other: Self) -> bool

        interface Signed:
            def abs(self) -> Self

        struct Point:
            x = 0

        def twice[T: Addable](x: T) -> T:
            return x + x

        def size[T: Signed](x: T) -> T:
            return abs(x) + x

        def first[T: Before](xs: list[T]) -> T:
            return min(xs)

        def any_first[T](xs: list[T]) -> T:
            return max(xs)

        def main(stdout: Stdout):
            stdout.print(twice(1) + 'a', twice('a') + 1, size(1.5) + 'a')
            stdout.print(twice(True), twice(Point()), size('a'), first([1]) + 'a', first([Point()]))
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on a T and a T @ abs(x) + x",
            "`max` compares its items with `<`, but `T` could be any type @ max(xs)",
            "can't use `+` on an int and a str @ twice(1) + \"a\"",
            "can't use `+` on a str and an int @ twice(\"a\") + 1",
            "can't use `+` on a float and a str @ size(1.5) + \"a\"",
            "a bool isn't an `Addable`: it has no `+(self, other: Self) -> Self` @ twice(True)",
            "`Point` isn't an `Addable`: it has no `+(self, other: Self) -> Self` @ twice(Point())",
            "a str isn't a `Signed`: it has no `abs(self) -> Self` @ size(\"a\")",
            "can't use `+` on an int and a str @ first([1]) + \"a\"",
            "`Point` isn't a `Before`: it has no `<(self, other: Self) -> bool` @ first([Point()])",
        ]
    );
}

#[test]
fn a_programs_type_replaces_a_standard_library_type_of_the_same_name() {
    let program = "
        struct Tree:
            size: int

        struct TreeNode:
            name: str

        def main(stdout: Stdout):
            stdout.print(Tree(size=1).size, TreeNode(name='a').name)
    ";
    assert_eq!(problems(&program.replace('\'', "\"")), Vec::<String>::new());
    let wrong = "
        struct Tree:
            size: int

        def main(stdout: Stdout):
            stdout.print(len(Tree(size=1)))
    ";
    assert_eq!(problems(wrong), ["`Tree` has no method `len` @ len(Tree(size=1))"]);
}

#[test]
fn a_test_receives_capabilities_by_type() {
    let fine = "
        test \"all three\"(out: Stdout, files: Fs, time: Clock):
            out.print(files.exists(\"a\")?, time.now())
            text: str = out.captured()
            assert text == \"\"
    ";
    assert_eq!(problems(fine), Vec::<String>::new());
    let fix = "a test can receive `stdout: Stdout`, `stderr: Stderr`, `fs: Fs`, `clock: Clock`, `process: Process`, `entropy: Entropy`, `net: Net`, `env: Env` and `signals: Signals`";
    for (written, name) in
        [("args: Args", "args"), ("n: int", "n"), ("out: Stdout = 1", "out"), ("self", "self")]
    {
        let program = format!("test \"one\"({written}):\n    pass\n");
        assert_eq!(problems(&program), [format!("a test can't receive `{name}` this way @ {written}")]);
        assert_eq!(helps(&program), [fix]);
    }
    assert_eq!(problems("test \"one\"(fs: Fs):\n    fs.fly()\n"), ["`Fs` has no method `fly` @ fs.fly()"]);
    assert_eq!(problems("test \"one\":\n    fs.exists(\"a\")\n"), ["`fs` doesn't exist @ fs"]);
}

#[test]
fn only_a_tests_own_body_reads_what_it_printed() {
    let help = "only a test's own body can read what it printed, from the `Stdout` it received";
    let helper = "def read(out: Stdout) -> str:\n    return out.captured()\n";
    assert_eq!(problems(helper), ["`Stdout` has no method `captured` @ out.captured()"]);
    assert_eq!(helps(helper), [help]);
    assert_eq!(in_main("stdout.captured()"), ["`Stdout` has no method `captured` @ stdout.captured()"]);
    let sorted = "def read(out: Stdout) -> list[str]:\n    return out.sorted_lines()\n";
    assert_eq!(problems(sorted), ["`Stdout` has no method `sorted_lines` @ out.sorted_lines()"]);
    assert_eq!(helps(sorted), [help]);
    assert_eq!(
        in_main("stdout.sorted_lines()"),
        ["`Stdout` has no method `sorted_lines` @ stdout.sorted_lines()"]
    );
    assert_eq!(
        problems("test \"one\"(fs: Fs):\n    fs.captured()\n"),
        ["`Fs` has no method `captured` @ fs.captured()"]
    );
}

#[test]
fn hashable_and_ordered_imply_eq_but_other_bounds_dont() {
    let program = "
        interface Named:
            def name(self) -> str

        def hashed[T: Hashable](a: T, b: T) -> bool:
            return a == b

        def ordered[T: Ordered](a: T, b: T) -> bool:
            return a == b

        def named[T: Named](a: T, b: T) -> bool:
            return a == b
    ";
    assert_eq!(problems(program), ["`T` could be any type, so `==` might not work on it @ a == b"]);
}

#[test]
fn dicts_and_sets_have_methods_the_standard_library_writes() {
    assert_eq!(
        in_main(
            "
            mut scores = {'a': 1}
            scores.update({'b': 2})
            total: int = scores.setdefault('c', 3)
            both: set[int] = {1}.union({2})
            same: bool = {1}.is_subset({1, 2})
            "
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        in_main("mut scores = {'a': 1}\nscores.update({'b': 'x'})"),
        ["`more` should be a dict[str, int], but this is a dict[str, str] @ {\"b\": \"x\"}"]
    );
    assert_eq!(in_main("scores = {'a': 1}\nscores.update({})"), ["`scores` isn't mut @ scores"]);
    assert_eq!(
        in_main("_ = {1}.union({'a'})"),
        ["`other` should be a set[int], but this is a set[str] @ {\"a\"}"]
    );
    assert_eq!(
        in_main("_ = {1}.is_subset([1])"),
        ["`other` should be a set[int], but this is a list[int] @ [1]"]
    );
}

#[test]
fn a_program_cant_extend_dict_or_set() {
    for (name, params) in [("dict", "K, V"), ("set", "T")] {
        let program = format!("extend {name}[{params}]:\n    def own(self) -> int:\n        1\n");
        assert_eq!(
            problems(&program),
            [format!("`{name}` is built in, so it can't be extended @ extend {name}[{params}]")]
        );
    }
}

const SOURCE: &str = "interface Source:
    def field[T](ref self, at: int) -> T?

struct Fake:
    n = 0

    def field[T](ref self, at: int) -> T?:
        self.n += at
        return None

def read[S: Source](ref src: S) -> int:
    return src.field[int](0) else 7

";

#[test]
fn a_method_takes_its_own_types_in_brackets() {
    let text = format!(
        "{SOURCE}struct Pair[A]:\n    first: A\n\n    def with_second[B](self, second: B) -> (A, B):\n        return (self.first, second)\n\ndef main(stdout: Stdout):\n    mut fake = Fake()\n    \
         stdout.print(read(ref fake), fake.field[str](2) else \"none\", Pair(first=1).with_second[str](\"two\"))\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
    let text = format!(
        "{SOURCE}def main(stdout: Stdout):\n    mut fake = Fake()\n    stdout.print(fake.field[int, str](1))\n"
    );
    assert_eq!(
        problems(&text),
        ["`field` takes 1 type in brackets, but this gives 2 @ fake.field[int, str](1)"]
    );
    let text = format!(
        "{SOURCE}def main(stdout: Stdout):\n    mut fake = Fake()\n    x: str = fake.field[int](1) else 0\n"
    );
    assert_eq!(problems(&text).len(), 1);
}

#[test]
fn an_interfaces_generic_method_needs_a_generic_method_to_fit() {
    let text = format!(
        "{SOURCE}struct Plain:\n    n = 0\n\n    def field(ref self, at: int) -> int?:\n        return None\n\n\
         def main(stdout: Stdout):\n    mut plain = Plain()\n    stdout.print(read(ref plain))\n"
    );
    assert_eq!(
        problems(&text),
        [
            "`Plain` isn't a `Source`: it has no `field[T](ref self, at: int) -> T?`. Its `field` returns an int? @ read(ref plain)"
        ]
    );
}

const AREAS: &str = "interface Shape:
    def area(self) -> float

struct Square:
    side = 1.0

    def area(self) -> float:
        return self.side * self.side

";

#[test]
fn a_parameter_typed_by_an_interface_takes_any_type_that_fits_it() {
    let text = format!(
        "{AREAS}def total(first: Shape, second: Shape) -> float:\n    return first.area() + second.area()\n\n\
         def main(stdout: Stdout):\n    stdout.print(total(Square(), Square(side=2.0)))\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
    let text = format!(
        "{AREAS}def main(stdout: Stdout):\n    stdout.print(total(5))\n\ndef total(first: Shape) -> float:\n    return first.area()\n"
    );
    assert_eq!(problems(&text), ["an int isn't a `Shape`: it has no `area(self) -> float` @ total(5)"]);
    let text = format!("{AREAS}def size(shape: Shape) -> float:\n    return shape.side\n");
    assert_eq!(problems(&text), ["`Shape` has no field `side` @ shape.side"]);
}

#[test]
fn an_interfaces_method_can_take_an_interface() {
    let saver = "interface Saver:\n    def save(self, shape: Shape) -> float\n\n\
                 struct Doubler:\n    def save(self, shape: Shape) -> float:\n        return shape.area() * 2.0\n\n\
                 struct Written:\n    def save[S: Shape](self, shape: S) -> float:\n        return shape.area()\n\n\
                 def keep(saver: Saver, shape: Shape) -> float:\n    return saver.save(shape)\n\n";
    let text = format!(
        "{AREAS}{saver}def main(stdout: Stdout):\n    stdout.print(keep(Doubler(), Square()), keep(Written(), Square()))\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
    let text = format!("{AREAS}{saver}def bad(saver: Saver) -> float:\n    return saver.save(5)\n");
    assert_eq!(problems(&text), ["an int isn't a `Shape`: it has no `area(self) -> float` @ saver.save(5)"]);
}

#[test]
fn a_method_cant_ask_more_of_its_type_parameters_than_the_interface_does() {
    let text = format!(
        "{AREAS}interface Keeper:\n    def keep[T](self, value: T) -> float\n\n\
         struct Picky:\n    def keep[T: Shape](self, value: T) -> float:\n        return value.area()\n\n\
         def hold[K: Keeper](keeper: K) -> float:\n    return keeper.keep(1)\n\n\
         def main(stdout: Stdout):\n    stdout.print(hold(Picky()))\n"
    );
    assert_eq!(
        problems(&text),
        [
            "`Picky` isn't a `Keeper`: it has no `keep[T](self, value: T) -> float`. Its `keep` asks more of its type parameters @ hold(Picky())"
        ]
    );
}

#[test]
fn brackets_only_name_the_type_parameters_that_are_written() {
    let shown = "def show[T](label: T, shape: Shape) -> float:\n    return shape.area()\n\n";
    let text =
        format!("{AREAS}{shown}def main(stdout: Stdout):\n    stdout.print(show[str](\"x\", Square()))\n");
    assert_eq!(problems(&text), Vec::<String>::new());
    let text = format!(
        "{AREAS}{shown}def main(stdout: Stdout):\n    stdout.print(show[str, Square](\"x\", Square()))\n"
    );
    assert_eq!(
        problems(&text),
        ["`show` takes 1 type in brackets, but this gives 2 @ show[str, Square](\"x\", Square())"]
    );
    let text = "def one[T](value: T) -> T:\n    return value\n\ndef main(stdout: Stdout):\n    stdout.print(one[int, str](1))\n";
    assert_eq!(problems(text), ["`one` takes 1 type in brackets, but this gives 2 @ one[int, str](1)"]);
}

/// A tuple type written alone in brackets is one type, as `show[(str, int)]`: the brackets'
/// several types are a tuple starting where its first type does.
#[test]
fn a_tuple_type_alone_in_brackets_is_one_type() {
    let shown = "def show[T](label: T, shape: Shape) -> float:\n    return shape.area()\n\n";
    let text = format!(
        "{AREAS}{shown}def main(stdout: Stdout):\n    stdout.print(show[(str, int)]((\"x\", 1), Square()))\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
    let text = format!(
        "{AREAS}{shown}def main(stdout: Stdout):\n    stdout.print(show[(str, int), Square]((\"x\", 1), Square()))\n"
    );
    assert_eq!(
        problems(&text),
        [
            "`show` takes 1 type in brackets, but this gives 2 @ show[(str, int), Square]((\"x\", 1), Square())"
        ]
    );
}

#[test]
fn an_interface_is_only_a_parameters_type_by_itself() {
    for written in [
        "def f(shape: Shape?):\n    pass\n",
        "def f(shapes: list[Shape]):\n    pass\n",
        "def f(shape: Square) -> Shape:\n    return shape\n",
        "struct Holder:\n    shape: Shape\n",
    ] {
        let found = problems(&format!("{AREAS}{written}"));
        assert!(
            found.iter().any(|problem| problem.starts_with("`Shape` is an interface, not a type")),
            "{found:?}"
        );
    }
}

/// The interfaces the standard library declares, which a program is free to name things after.
fn library_interfaces() -> Vec<String> {
    let library = fennec_syntax::prelude_modules().unwrap_or_default();
    let interfaces = fennec_syntax::visit::interface_names(library.iter());
    let found: Vec<String> = interfaces
        .into_iter()
        .filter(|name| !fennec_syntax::visit::BUILT_IN_INTERFACES.contains(&name.as_str()))
        .collect();
    assert_ne!(found, Vec::<String>::new());
    found
}

#[test]
fn a_programs_own_type_can_be_named_like_a_library_interface() {
    for name in library_interfaces() {
        let text = format!(
            "struct {name}:\n    width = 3\n\ndef wide(given: {name}) -> int:\n    given.width\n\n\
             def main(stdout: Stdout):\n    stdout.print(wide({name}()))\n"
        );
        assert_eq!(problems(&text), Vec::<String>::new());
    }
}

#[test]
fn a_programs_own_alias_can_be_named_like_a_library_interface() {
    for name in library_interfaces() {
        let text = format!("type {name} = int\n\ndef twice(given: {name}) -> int:\n    given * 2\n");
        assert_eq!(problems(&text), Vec::<String>::new());
    }
}

#[test]
fn a_programs_own_interface_can_be_named_like_a_library_interface() {
    for name in library_interfaces() {
        let text = format!(
            "interface {name}:\n    def width(self) -> int\n\nstruct Box:\n    size = 3\n\n    \
             def width(self) -> int:\n        self.size\n\n\
             struct Holder[T: {name}]:\n    item: T\n\n\
             def wide(given: {name}) -> int:\n    given.width()\n\n\
             def main(stdout: Stdout):\n    stdout.print(wide(Box()))\n    stdout.print(Holder(item=Box()).item.width())\n"
        );
        assert_eq!(problems(&text), Vec::<String>::new());
    }
}

const READS: &str =
    "error Bad:\n    message: str\n\ninterface Reads:\n    def next(ref self) -> int fails Bad\n\n";

#[test]
fn a_method_of_an_interface_can_say_how_it_fails() {
    let fits = "struct Counter:\n    at = 0\n\n    def next(ref self) -> int fails Bad:\n        self.at += 1\n        return self.at\n\n";
    let text = format!(
        "{READS}{fits}def two(ref source: Reads) -> int fails Bad:\n    return source.next()? + source.next()?\n\n\
         def main(stdout: Stdout):\n    mut counter = Counter()\n    counted = two(ref counter) except Bad(): 0\n    stdout.print(counted)\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
    // What it gives back can fail, so `?` or `except` has to say what happens.
    let text = format!("{READS}{fits}def one(ref source: Reads) -> int:\n    return source.next()\n");
    assert_eq!(problems(&text).len(), 1, "{:?}", problems(&text));
    let text = "interface Reads:\n    def next(ref self) -> int fails Nope\n";
    assert_eq!(problems(text), ["`Nope` isn't a type @ Nope"]);
}

#[test]
fn a_type_has_to_fail_as_its_interfaces_method_says() {
    let text = format!(
        "{READS}struct Sure:\n    def next(ref self) -> int:\n        return 1\n\n\
         def two(ref source: Reads) -> int fails Bad:\n    return source.next()?\n\n\
         def main(stdout: Stdout):\n    mut sure = Sure()\n    counted = two(ref sure) except Bad(): 0\n    stdout.print(counted)\n"
    );
    let found = problems(&text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found.iter().all(|problem| problem.contains("`next(ref self) -> int fails Bad`")), "{found:?}");
}

#[test]
fn self_reaches_an_extension_and_a_matchs_arms() {
    let text = "struct Point:\n    x = 1\n\nextend Point:\n    def again(self) -> Self:\n        return Self(x=self.x)\n\n\
                enum Shape:\n    Dot\n    Line(length: int)\n\n    def same(self) -> Self:\n        match self:\n            \
                case .Line(length):\n                made = Self.Line(length=length)\n                return made\n            \
                case .Dot:\n                dot = Self.Dot\n                return dot\n\n\
                def main(stdout: Stdout):\n    stdout.print(Point().again().x, Shape.Dot.same())\n";
    assert_eq!(problems(text), Vec::<String>::new());
    // An extension's `Self` is its type, so it doesn't fit what's another.
    let text = "struct Point:\n    x = 1\n\nextend Point:\n    def again(self) -> Self:\n        return 1\n";
    assert_eq!(problems(text).len(), 1, "{:?}", problems(text));
}

#[test]
fn self_in_a_types_own_method_is_that_type() {
    let text = "struct Point:\n    x = 1\n\n    def make() -> Self:\n        return Self(x=7)\n\n    def same(self, other: Self) -> list[Self]:\n        \
                held: Self = other\n        return [held, self]\n\n\
                struct Box[T]:\n    item: T\n\n    def again(self) -> Self:\n        return Box(item=self.item)\n\n\
                def main(stdout: Stdout):\n    stdout.print(Point.make().x, Box(item=1).again().item)\n";
    assert_eq!(problems(text), Vec::<String>::new());
    let text = "struct Point:\n    x = 1\n\n    def make() -> Self:\n        return 1\n";
    assert_eq!(problems(text).len(), 1, "{:?}", problems(text));
    // Outside a method, nothing is `Self`.
    let text = "def same(x: Self) -> int:\n    return 1\n";
    assert_eq!(problems(text), ["`Self` isn't a type @ Self"]);
}

const MADE: &str = "interface Made:\n    def make() -> Self\n\nstruct Part:\n    x = 1\n\n    def make() -> Part:\n        return Part(x=7)\n\n";

#[test]
fn a_type_parameter_has_its_interfaces_functions() {
    let text = format!(
        "{MADE}def build[T: Made]() -> T:\n    return T.make()\n\ndef main(stdout: Stdout):\n    stdout.print(build[Part]().x)\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
    let text = format!("{MADE}def build[T: Made]() -> T:\n    return T.other()\n");
    assert_eq!(problems(&text), ["`T` has nothing called `other` @ T.other()"]);
    let text = format!("{MADE}def build[T]() -> T:\n    return T.make()\n");
    assert_eq!(problems(&text), ["`T` has nothing called `make` @ T.make()"]);
    let text = format!("{MADE}def build[T: Made]() -> int:\n    return T.make()\n");
    assert_eq!(problems(&text).len(), 1, "{:?}", problems(&text));
    let text =
        "interface Sized:\n    def size(self) -> int\n\ndef build[T: Sized]() -> int:\n    return T.size()\n";
    assert_eq!(problems(text), ["`size` needs a `T` to call it on @ T.size()"]);
    let text = "def build() -> int:\n    return T.make()\n";
    assert_eq!(problems(text).len(), 1, "{:?}", problems(text));
}

/// A function with no `self` on a generic type asks of the type's own parameters, which the type
/// fitting the interface says: a `Pile[Part]` fits, and a `Pile[Rock]` doesn't.
#[test]
fn a_generic_types_function_fits_an_interface_by_what_its_parameters_are() {
    let source = "
interface Made:
    def make() -> Self

struct Part:
    x = 1

    def make() -> Self:
        return Self(x=7)

struct Rock:
    weight = 1

struct Pile[T: Made]:
    items: list[T]

    def make() -> Self:
        return Pile[T](items=[T.make()])

def build[T: Made]() -> T:
    return T.make()
";
    let fits =
        format!("{source}\ndef main(stdout: Stdout):\n    stdout.print(build[Pile[Part]]().items.len())\n");
    assert_eq!(problems(&fits), Vec::<String>::new());
    let misfits =
        format!("{source}\ndef main(stdout: Stdout):\n    stdout.print(build[Pile[Rock]]().items.len())\n");
    let found = problems(&misfits);
    assert!(found.iter().any(|problem| problem.contains("isn't a `Made`")), "{found:?}");
}

#[test]
fn a_method_that_changes_its_value_doesnt_fit_an_interfaces_self() {
    let source = "interface Source:\n    def next(self) -> int\n\n\
                  struct Count:\n    left = 2\n\n    def next(ref self) -> int:\n        self.left -= 1\n        return self.left\n\n\
                  def two(source: Source) -> int:\n    return source.next() + source.next()\n\n\
                  def main(stdout: Stdout):\n    counter = Count()\n    stdout.print(two(counter))\n";
    let found = problems(source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("it has no `next(self) -> int`. Its `next` changes `self`"), "{found:?}");
    let helped = helps(source);
    assert!(
        helped.iter().any(|help| help.contains("write `ref self` in the interface's `next`")),
        "{helped:?}"
    );
}

#[test]
fn an_interfaces_mut_self_fits_a_method_that_changes_it_or_one_that_doesnt() {
    let empty: [&str; 0] = [];
    let source = "interface Source:\n    def next(ref self) -> int\n\n\
                  struct Count:\n    left = 2\n\n    def next(ref self) -> int:\n        self.left -= 1\n        return self.left\n\n\
                  struct Fixed:\n    held = 7\n\n    def next(self) -> int:\n        return self.held\n\n\
                  def two(ref source: Source) -> int:\n    return source.next() + source.next()\n\n\
                  def main(stdout: Stdout):\n    mut counter = Count()\n    mut fixed = Fixed()\n    \
                  stdout.print(two(ref counter), counter.left, two(ref fixed))\n";
    assert_eq!(problems(source), empty);
}

#[test]
fn a_function_with_no_self_has_its_first_parameter_compared_too() {
    let source = "interface Made:\n    def make(size: int) -> Self\n\n\
                  struct Part:\n    size = 1\n\n    def make(size: str) -> Self:\n        return Self(size=len(size))\n\n\
                  def build[T: Made]() -> T:\n    return T.make(1)\n\n\
                  def main(stdout: Stdout):\n    stdout.print(build[Part]().size)\n";
    let found = problems(source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("takes other arguments"), "{found:?}");
}

#[test]
fn what_might_be_missing_can_be_saved_when_what_it_holds_can() {
    let empty: [&str; 0] = [];
    let types = "struct Plain:\n    work: (int) -> int\n\ndef plains() -> list[Plain]:\n    return []\n\ndef saved[T: Serializable](x: T) -> int:\n    return 1\n\ndef loaded[T: Deserializable]() -> int:\n    return 1\n";
    let body =
        "words: list[str?] = [None]\nstdout.print(saved(words), loaded[int?](), loaded[list[str?]]())\n";
    assert_eq!(crate::in_main_after(types, body), empty);
    let found = crate::in_main_after(types, "plains: list[Plain?] = [None]\nstdout.print(saved(plains))\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("isn't a `Serializable`"), "{found:?}");
    let found = crate::in_main_after(types, "stdout.print(saved(plains()))\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("isn't a `Serializable`"), "{found:?}");
}

const SAVED: &str = "struct Spot:
    x = 0

struct Holder:
    spot: Spot
    label: str?
    spots = list[Spot]()

struct Listener:
    on_change: (int) -> int
    size = 1

struct Inventory:
    listener: Listener
    items = list[int]()

struct Character:
    inventory: Inventory

struct Cell:
    count = 0

struct Celled:
    cell: shared Cell

struct Quiet:
    @skip
    cell: (shared Cell)? = None
    size = 1

def saved[T: Serializable](x: T) -> int:
    return 1

def loaded[T: Deserializable]() -> int:
    return 1
";

#[test]
fn a_type_saves_and_loads_when_all_its_fields_can() {
    let empty: [&str; 0] = [];
    let body = "stdout.print(loaded[Spot](), loaded[Holder](), loaded[list[Holder]](), loaded[Quiet]())\n";
    assert_eq!(crate::in_main_after(SAVED, body), empty);
}

#[test]
fn what_cant_be_saved_names_where_it_is_and_the_fixes() {
    let found = crate::in_main_after(SAVED, "stdout.print(loaded[Character]())\n");
    assert_eq!(
        found,
        [
            "`Character` isn't a `Deserializable`: `Character.inventory.listener.on_change` is a function, which can't be saved @ loaded[Character]()"
        ]
    );
    let helped =
        helps(&format!("{SAVED}\ndef main(stdout: Stdout):\n    stdout.print(loaded[Character]())\n"));
    assert_eq!(
        helped,
        [
            "`@skip` the field `on_change`, with a default, or write `def serialize` and `def deserialize` for `Listener`"
        ]
    );
    let found = crate::in_main_after(SAVED, "stdout.print(loaded[Celled]())\n");
    assert_eq!(
        found,
        [
            "`Celled` isn't a `Deserializable`: `Celled.cell` is `shared`, which can't be saved @ loaded[Celled]()"
        ]
    );
    // A collection that can't be saved says which field of what it holds is why.
    let found = crate::in_main_after(SAVED, "stdout.print(loaded[list[Celled]]())\n");
    assert_eq!(
        found,
        [
            "a list[Celled] isn't a `Deserializable`: `Celled.cell` is `shared`, which can't be saved @ loaded[list[Celled]]()"
        ]
    );
}

/// A union is saved tagged with its member, so each member has to be told apart by its type's
/// name, and a set's items and a dict's keys can't hold one.
#[test]
fn a_union_saves_when_its_members_are_named_types() {
    let types = "struct Circle:\n    r = 1.0\n\nstruct Scene:\n    main: Circle | int\n    all: list[Circle | str]\n    named: dict[str, Circle | int]\n    picked: (Circle | int)? = None\n\nstruct Mixed:\n    held: list[int] | str\n\nstruct Keyed:\n    by: dict[int | str, int]\n\ndef loaded[T: Deserializable]() -> int:\n    return 1\n";
    let empty: [&str; 0] = [];
    assert_eq!(crate::in_main_after(types, "stdout.print(loaded[Scene]())\n"), empty);
    assert_eq!(
        crate::in_main_after(types, "stdout.print(loaded[Mixed]())\n"),
        [
            "`Mixed` isn't a `Deserializable`: `Mixed.held` is a list[int] | str, which can't be saved @ loaded[Mixed]()"
        ]
    );
    assert_eq!(
        crate::in_main_after(types, "stdout.print(loaded[Keyed]())\n"),
        [
            "`Keyed` isn't a `Deserializable`: `Keyed.by` is a dict[int | str, int], which can't be saved @ loaded[Keyed]()"
        ]
    );
}

/// A tuple saves when each of its items does, but not one of a single item yet.
#[test]
fn a_tuple_saves_when_its_items_do() {
    let types = "struct Holder:\n    run: () -> int\n\ndef loaded[T: Deserializable]() -> int:\n    return 1\n\ndef saved[T: Serializable](held: T) -> int:\n    return 1\n";
    let empty: [&str; 0] = [];
    assert_eq!(crate::in_main_after(types, "stdout.print(loaded[(int, str?),]())\n"), empty);
    let found = crate::in_main_after(types, "stdout.print(loaded[(int, Holder),]())\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`Holder` is one of its items"), "{found:?}");
    let found = crate::in_main_after(types, "stdout.print(saved((1,)))\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("is a tuple of one item, which can't be saved yet"), "{found:?}");
    let helped = helps(&format!("{types}\ndef main(stdout: Stdout):\n    stdout.print(saved((1,)))\n"));
    assert_eq!(helped, ["save the item itself"]);
}

#[test]
fn a_type_writes_both_or_neither() {
    let one =
        "struct Spot:\n    x = 0\n\n    def serialize(self, ref out: Serializer):\n        out.int(self.x)\n";
    assert_eq!(problems(one), ["`Spot` writes `def serialize` but not `def deserialize` @ struct Spot"]);
    let other = "struct Spot:\n    x = 0\n\n    def deserialize(ref src: Deserializer) -> Self fails ParseError:\n        return Spot(x=src.int()?)\n";
    assert_eq!(problems(other), ["`Spot` writes `def deserialize` but not `def serialize` @ struct Spot"]);
    let empty: [&str; 0] = [];
    let both = "struct Spot:\n    x = 0\n\n    def serialize(self, ref out: Serializer):\n        out.int(self.x)\n\n    def deserialize(ref src: Deserializer) -> Self fails ParseError:\n        return Spot(x=src.int()?)\n";
    assert_eq!(problems(both), empty);
}

#[test]
fn a_program_that_doesnt_save_isnt_given_the_methods() {
    let source = "struct Spot:\n    x = 0\n\ndef main(stdout: Stdout):\n    stdout.print(Spot().x)\n";
    let signatures = crate::check(source).unwrap();
    assert!(signatures.get("Spot.serialize").is_none());
    assert!(signatures.get("Spot.deserialize").is_none());
}

/// A loop over `fields(T)` gives each field's `name` and `get(x)`, checked once: the value of a
/// field goes only where any type can, and nothing else is asked of a field.
#[test]
fn a_loop_over_fields_is_checked_once() {
    let types = "def take[V](value: V) -> int:\n    return 1\n\n";
    let body = |inside: &str| {
        problems(&format!(
            "{types}def walk[T](value: T) -> int:\n    mut count = 0\n    for field in fields(T):\n        {inside}\n    return count\n\n\
             def main(stdout: Stdout):\n    stdout.print(walk(3))\n"
        ))
        .into_iter()
        .map(|problem| problem.split(" @ ").next().unwrap_or_default().to_owned())
        .collect::<Vec<String>>()
    };
    assert_eq!(body("count += take(field.get(value)) + len(field.name)"), Vec::<String>::new());
    assert_eq!(body("count += field.size"), ["a field has a `name` and a `get(...)`, not `size`"]);
    assert_eq!(body("count += take(field.get(1.5))"), ["`get` reads a field of a T, not a float"]);
    assert_eq!(body("count += field.get(value) + 1"), ["can't use `+` on a FieldValue and an int"]);
}
