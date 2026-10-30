//! Generic functions and types, and interfaces. A type parameter stands for one type per
//! use, worked out from the arguments or written in brackets. Its bounds say what that type
//! has to be able to do, and inside the generic code, all a value of it can do. A type fits
//! an interface by having its methods; `Eq`, `Hashable`, `Ordered` and `Format` are built in.

use fennec_check::Type;

use crate::{helps, problems, returns};

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
            items: list[T] = []

            def push(mut self, item: T):
                self.items.append(item)

            def top(self) -> T?:
                if len(self.items) == 0:
                    return None
                self.items[-1]

        struct Ranking[T: Ordered]:
            best: list[T] = []

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
        "struct Stack[T]:\n    items: list[T] = []\n\ndef main(stdout: Stdout):\n    empty = Stack()\n";
    assert_eq!(problems(program), ["can't tell what `T` is in this call @ Stack()"]);
}

#[test]
fn generic_types_need_their_types_written() {
    let program = "
        struct Stack[T]:
            items: list[T] = []

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
