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
        self.side * self.side

struct Rock:
    weight = 1

    def area(self) -> int:
        1
";

fn with_shapes(program: &str) -> Vec<String> {
    problems(&format!("{SHAPES}\n{}", crate::dedent(program)))
}

#[test]
fn type_parameters_are_chosen_by_each_call() {
    let program = "
        def first[T](items: list[T]) -> T:
            items[0]

        def pair[A, B](a: A, b: B) -> (A, B):
            (a, b)

        def main(stdout: Stdout):
            stdout.print(first([1]) + 1, first(['a']).upper(), pair(1, 'a')[1].upper())
            stdout.print(first([1]) + 'a')
    ";
    assert_eq!(problems(program), ["can't use `+` on an int and a str @ first([1]) + \"a\""]);
    let program = "def first[T](items: list[T]) -> T:\n    items[0]\n";
    assert_eq!(returns(program, "first"), Ok(Type::Param("T".to_owned())));
}

#[test]
fn a_type_parameter_fits_only_one_type_per_call() {
    let program = "
        def same[T](a: T, b: T) -> T:
            a

        def main(stdout: Stdout):
            stdout.print(same(1, 'a'))
    ";
    assert_eq!(problems(program), ["`b` should be an int, but this is a str @ \"a\""]);
}

#[test]
fn types_can_be_written_in_brackets_when_nothing_shows_them() {
    let program = "
        def make[T]() -> list[T]:
            []

        def main(stdout: Stdout):
            stdout.print(make())
            numbers: list[int] = make()
            stdout.print(make[str](), make[int, str]())
    ";
    assert_eq!(problems(program), ["can't tell what `T` is in this call @ make()"]);
}

#[test]
fn values_of_a_type_parameter_can_do_only_what_its_bounds_say() {
    let program = "
        def total[T: Shape](shapes: list[T]) -> float:
            sum(shape.area() for shape in shapes)

        def wrong[T: Shape](shape: T):
            shape.perimeter()

        def unbound[T](value: T):
            value.area()

        def compare[T](a: T, b: T) -> bool:
            a < b or a == b

        def ordered[T: Ordered](a: T, b: T) -> bool:
            a < b and a == b

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
            0.0
    ";
    assert_eq!(with_shapes(program), ["`Shape` is an interface, not a type @ Shape"]);
    assert_eq!(
        helps(&format!("{SHAPES}\ndef f(shape: Shape):\n    pass\n")),
        [
            "take a type parameter that fits it, like `def f[T: Shape](items: list[T])`, or use an enum for a mix of types"
        ]
    );
}

#[test]
fn calls_check_their_types_fit_the_bounds() {
    let program = "
        def total[T: Shape](shapes: list[T]) -> float:
            sum(shape.area() for shape in shapes)

        def largest[T: Ordered & Hashable](items: list[T]) -> T:
            max(items)

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
            max(shape.area() for shape in shapes)

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
            shape.area()

        def total[T: Shape](shapes: list[T]) -> float:
            sum(area_of(shape) for shape in shapes)

        def loose[T](value: T) -> float:
            area_of(value)

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

            def push(mut self, item: T):
                self.items.append(item)

            def top(self) -> T?:
                if self.items.len() == 0:
                    return None
                self.items[-1]

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
            f(n)

        def combine(f: (int, int) -> int) -> int:
            f(1, 2)

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
            x + x

        def size[T: Signed](x: T) -> T:
            x.abs() + x

        def first[T: Before](xs: list[T]) -> T:
            min(xs)

        def any_first[T](xs: list[T]) -> T:
            max(xs)

        def main(stdout: Stdout):
            stdout.print(twice(1) + 'a', twice('a') + 1, size(1.5) + 'a')
            stdout.print(twice(True), twice(Point()), size('a'), first([1]) + 'a', first([Point()]))
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on a T and a T @ x.abs() + x",
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
            stdout.print(Tree(size=1).len())
    ";
    assert_eq!(problems(wrong), ["`Tree` has no method `len` @ Tree(size=1).len()"]);
}

#[test]
fn a_test_receives_capabilities_by_type() {
    let fine = "
        test \"all three\"(out: Stdout, files: Fs, time: Clock):
            out.print(files.exists(\"a\"), time.now())
            text: str = out.captured()
            assert text == \"\"
    ";
    assert_eq!(problems(fine), Vec::<String>::new());
    let fix = "a test can receive `stdout: Stdout`, `fs: Fs` and `clock: Clock`";
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
    let helper = "def read(out: Stdout) -> str:\n    out.captured()\n";
    assert_eq!(problems(helper), ["`Stdout` has no method `captured` @ out.captured()"]);
    assert_eq!(helps(helper), [help]);
    assert_eq!(in_main("stdout.captured()"), ["`Stdout` has no method `captured` @ stdout.captured()"]);
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
            a == b

        def ordered[T: Ordered](a: T, b: T) -> bool:
            a == b

        def named[T: Named](a: T, b: T) -> bool:
            a == b
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
        in_main("{1}.union({'a'})"),
        ["`other` should be a set[int], but this is a set[str] @ {\"a\"}"]
    );
    assert_eq!(
        in_main("{1}.is_subset([1])"),
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
