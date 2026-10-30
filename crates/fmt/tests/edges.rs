//! Edges of the style that mutation testing showed weren't pinned down: exact widths,
//! what stops a list hugging, and names that are only sometimes keywords.

use crate::style::check;

#[test]
fn a_guard_clause_that_just_fits_stays_on_one_line() {
    check(
        "def f(x: int):\n    if x > 0: return bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        "def f(x: int):\n    if x > 0: return bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
    );
    check(
        "def f(x: int):\n    if x > 0: return bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        "def f(x: int):\n    if x > 0:\n        return bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
    );
}

#[test]
fn a_blank_line_before_a_last_comment_stays() {
    check("def f():\n    pass\n\n# end\n", "def f():\n    pass\n\n# end\n");
}

#[test]
fn fields_stay_together() {
    check(
        "struct A:\n    x: int\n    y: int\n\n    def get(self):\n        self.x\n",
        "struct A:\n    x: int\n    y: int\n\n    def get(self):\n        self.x\n",
    );
}

#[test]
fn a_long_one_line_function_body_moves_below() {
    check(
        "def area_of_the_shape(shape: Shape) -> float: return shape.width_of_the_shape * shape.height_of_the_shape\n",
        "def area_of_the_shape(shape: Shape) -> float:\n    return shape.width_of_the_shape * shape.height_of_the_shape\n",
    );
}

#[test]
fn a_long_one_line_if_body_moves_below() {
    check(
        "def f(x: int):\n    if x > 0: stdout.print(\"this line is long enough that the body has to move down to its own line\")\n",
        "def f(x: int):\n    if x > 0:\n        stdout.print(\"this line is long enough that the body has to move down to its own line\")\n",
    );
}

#[test]
fn a_field_named_case_is_not_a_match_arm() {
    check(
        "struct S:\n    case: list[int] = [1000000000, 2000000000, 3000000000, 4000000000, 5000000000, 6000000000, 70000]\n",
        "struct S:\n    case: list[int] = [\n        1000000000, 2000000000, 3000000000, 4000000000, 5000000000, 6000000000, 70000,\n    ]\n",
    );
}

#[test]
fn a_long_header_keeps_its_comment() {
    check(
        "def spawn(world: World, position: Vec3, velocity: Vec3, health: int, display_name: str, x: int, y: int):  # why\n    pass\n",
        "def spawn(\n    world: World,\n    position: Vec3,\n    velocity: Vec3,\n    health: int,\n    display_name: str,\n    x: int,\n    y: int,\n):  # why\n    pass\n",
    );
}

#[test]
fn interface_signatures_with_typed_parameters_stay_together() {
    check(
        "interface Shape:\n    def scale(self, by: float) -> Shape\n    def area(self) -> float\n",
        "interface Shape:\n    def scale(self, by: float) -> Shape\n    def area(self) -> float\n",
    );
}

#[test]
fn an_item_one_past_the_width_breaks() {
    check(
        "def f():\n    outer(first_argument_that_is_long, g(argument_one, argument_two, xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx))\n",
        "def f():\n    outer(\n        first_argument_that_is_long,\n        g(\n            argument_one,\n            argument_two,\n            xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx,\n        ),\n    )\n",
    );
}

#[test]
fn an_item_that_just_fits_stays() {
    check(
        "def f():\n    outer(first_argument_that_is_long, g(argument_one, argument_two, xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx))\n",
        "def f():\n    outer(\n        first_argument_that_is_long,\n        g(argument_one, argument_two, xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx),\n    )\n",
    );
}

#[test]
fn a_short_list_early_in_a_long_line_stays() {
    check(
        "def f():\n    x = first(a).then(first_long_argument_name, second_long_argument_name, third_long_argument_name1)\n",
        "def f():\n    x = first(a).then(\n        first_long_argument_name,\n        second_long_argument_name,\n        third_long_argument_name1,\n    )\n",
    );
}

#[test]
fn type_arguments_break_without_a_trailing_comma() {
    check(
        "def lookup() -> dict[AVeryLongTypeNameForTheKeysOfThisTable, AnotherVeryLongTypeNameForTheValuesHere]:\n    pass\n",
        "def lookup() -> dict[\n    AVeryLongTypeNameForTheKeysOfThisTable,\n    AnotherVeryLongTypeNameForTheValuesHere\n]:\n    pass\n",
    );
}

#[test]
fn a_trailing_comma_doesnt_stop_hugging() {
    check(
        "def f():\n    g([\"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\", \"item number 5\", \"item number 6\", \"item number 7\"],)\n",
        "def f():\n    g([\n        \"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\",\n        \"item number 5\", \"item number 6\", \"item number 7\",\n    ])\n",
    );
}

#[test]
fn a_comment_after_the_bracket_stops_hugging() {
    check(
        "def f():\n    g(  # why\n        [\"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\", \"item number 5\", \"item number 6\", \"item number 7\"]\n    )\n",
        "def f():\n    g(  # why\n        [\n            \"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\",\n            \"item number 5\", \"item number 6\", \"item number 7\",\n        ],\n    )\n",
    );
}

#[test]
fn a_comment_after_the_item_stops_hugging() {
    check(
        "def f():\n    g(\n        [\"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\", \"item number 5\", \"item number 6\", \"item number 7\"]  # why\n    )\n",
        "def f():\n    g(\n        [\n            \"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\",\n            \"item number 5\", \"item number 6\", \"item number 7\",\n        ],  # why\n    )\n",
    );
}

#[test]
fn a_comment_before_the_bracket_stops_hugging() {
    check(
        "def f():\n    g(\n        [\"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\", \"item number 5\", \"item number 6\", \"item number 7\"]\n        # why\n    )\n",
        "def f():\n    g(\n        [\n            \"item number 0\", \"item number 1\", \"item number 2\", \"item number 3\", \"item number 4\",\n            \"item number 5\", \"item number 6\", \"item number 7\",\n        ],\n        # why\n    )\n",
    );
}

#[test]
fn an_empty_list_isnt_hugged() {
    check(
        "def f():\n    aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = f([])\n",
        "def f():\n    aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = f(\n        [],\n    )\n",
    );
}

#[test]
fn a_long_assignment_wraps_its_value_in_parentheses() {
    check(
        "def f():\n    value = compute(first_argument, second_argument) + another_long_name_to_add + one_more_name + more\n",
        "def f():\n    value = (\n        compute(first_argument, second_argument) + another_long_name_to_add + one_more_name + more\n    )\n",
    );
}

#[test]
fn long_operator_parameters_get_a_trailing_comma() {
    check(
        "struct Money:\n    cents: int\n\n    def -(self_with_a_rather_long_parameter_name: MoneyWithAVeryLongTypeName) -> MoneyAgainWithALongName:\n        self\n",
        "struct Money:\n    cents: int\n\n    def -(\n        self_with_a_rather_long_parameter_name: MoneyWithAVeryLongTypeName,\n    ) -> MoneyAgainWithALongName:\n        self\n",
    );
}

#[test]
fn arrow_function_parameters_never_get_a_trailing_comma() {
    check(
        "def f():\n    pick = (first_parameter_with_long_name, second_parameter_with_long_name) => first_parameter_with_long_name\n",
        "def f():\n    pick = (\n        first_parameter_with_long_name,\n        second_parameter_with_long_name\n    ) => first_parameter_with_long_name\n",
    );
}

#[test]
fn true_false_and_none_are_values() {
    check(
        "def f():\n    x = True - 1\n    y = [None] if False else None\n",
        "def f():\n    x = True - 1\n    y = [None] if False else None\n",
    );
}

#[test]
fn case_outside_a_match_is_a_name() {
    check("def f():\n    case(1)\n", "def f():\n    case(1)\n");
}

#[test]
fn where_outside_a_field_rule_is_a_name() {
    check(
        "def f():\n    x = where(1)\n\nstruct S:\n    w = where(1)\n",
        "def f():\n    x = where(1)\n\nstruct S:\n    w = where(1)\n",
    );
}

#[test]
fn a_slice_after_mut_has_no_spaces() {
    check("def f(xs: list[int]):\n    mut ys = xs[1:3]\n", "def f(xs: list[int]):\n    mut ys = xs[1:3]\n");
}
