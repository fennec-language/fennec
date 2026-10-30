//! Generic functions and interfaces. A type parameter stands for one type per call, chosen
//! from the arguments. An interface written as a parameter's type means any one type that
//! has the interface's methods, so it's a type parameter too. A type fits an interface by
//! having its methods; there's nothing to declare.

use fennec_check::Type;

use crate::{helps, problems, returns};

const SHAPES: &str = "
interface Shape:
    def area(self) -> float

struct Circle:
    radius: float

    def area(self):
        PI * self.radius * self.radius

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
fn a_type_parameter_nothing_shows_is_a_mistake() {
    let program = "
        def make[T]() -> list[T]:
            []

        def main(stdout: Stdout):
            stdout.print(make())
            numbers: list[int] = make()
    ";
    assert_eq!(problems(program), ["can't tell what `T` is in this call @ make()"]);
}

#[test]
fn values_of_a_type_parameter_have_only_their_interface_s_methods() {
    let program = "
        def total[T: Shape](shapes: list[T]) -> float:
            sum(shape.area() for shape in shapes)

        def sizes(shapes: list[Shape]):
            [shape.area() for shape in shapes]

        def wrong[T: Shape](shape: T):
            shape.perimeter()

        def unbound[T](value: T):
            value.area()

        def main(stdout: Stdout):
            stdout.print(total([Circle(radius=1.0)]), sizes([Square(side=2.0)])[0] + 1.0)
    ";
    assert_eq!(
        with_shapes(program),
        ["`T` has no method `perimeter` @ shape.perimeter()", "`T` has no method `area` @ value.area()"]
    );
    assert_eq!(
        helps(&format!("{SHAPES}\ndef unbound[T](value: T):\n    value.area()\n")),
        ["bind `T` to an interface that has it, like `T: HasLength`"]
    );
}

#[test]
fn calls_check_their_types_fit_the_interface() {
    let program = "
        def total[T: Shape](shapes: list[T]) -> float:
            sum(shape.area() for shape in shapes)

        def biggest(shapes: list[Shape]) -> float:
            max(shape.area() for shape in shapes)

        def main(stdout: Stdout):
            stdout.print(total([Rock()]), biggest([1]), total([Circle(radius=1.0), Circle(radius=2.0)]))
    ";
    assert_eq!(
        with_shapes(program),
        [
            "`Rock` isn't `Shape`: its `area` method doesn't match `Shape`'s @ total([Rock()])",
            "an int isn't `Shape`: it has no `area` method @ biggest([1])",
        ]
    );
    assert_eq!(
        helps(&format!(
            "{SHAPES}\ndef total(shapes: list[Shape]):\n    0\n\ndef main(stdout: Stdout):\n    total([1])\n"
        )),
        ["give an int the method `def area(self) -> float`"]
    );
}

#[test]
fn one_call_holds_one_type_for_an_interface() {
    let program = "
        def biggest(shapes: list[Shape]) -> float:
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
    assert_eq!(with_shapes(program), ["`T` isn't `Shape`: `T` could be any type @ area_of(value)"]);
}

#[test]
fn types_cannot_have_type_parameters_yet() {
    assert_eq!(problems("struct Box[T]:\n    value: T\n"), ["parse: `Box` can't have type parameters yet"]);
}
