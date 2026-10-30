//! The style itself: each case is messy input and the one way it's formatted.

/// Checks `input` formats to `expected`, and that `expected` is already formatted.
#[track_caller]
pub(crate) fn check(input: &str, expected: &str) {
    assert_eq!(fennec_fmt::format(input), Ok(expected.to_owned()));
    assert_eq!(fennec_fmt::format(expected), Ok(expected.to_owned()), "formatting twice changed it");
}

#[test]
fn spaces_go_around_operators_and_after_commas() {
    check(
        "def f(a:int,b:int)->int:\n    x=-a+b*2**-3//4%5\n    y = a  if a>b  else -(b)\n    x+=1\n    x\n",
        "def f(a: int, b: int) -> int:\n    x = -a + b * 2 ** -3 // 4 % 5\n    y = a if a > b else -(b)\n    x += 1\n    x\n",
    );
    check(
        "def f(xs: list[int]):\n    ok = not xs and 3 not in xs or xs is not None\n",
        "def f(xs: list[int]):\n    ok = not xs and 3 not in xs or xs is not None\n",
    );
}

#[test]
fn calls_indexes_and_attributes_hug() {
    check(
        "def f(xs: list[int]):\n    g (xs [0], xs [1 : 3], xs[-2 :], xs[: 2]) . h ( )\n",
        "def f(xs: list[int]):\n    g(xs[0], xs[1:3], xs[-2:], xs[:2]).h()\n",
    );
    check(
        "def f(p: Player?):\n    p ?. name\n    load() ? from Failed(why = \"x\")\n",
        "def f(p: Player?):\n    p?.name\n    load()? from Failed(why=\"x\")\n",
    );
}

#[test]
fn keyword_arguments_hug_their_equals() {
    check(
        "def f():\n    Vec3(x = 1.0, y = 2.0)\n    sorted(names, key = name => name.len())\n",
        "def f():\n    Vec3(x=1.0, y=2.0)\n    sorted(names, key=name => name.len())\n",
    );
}

#[test]
fn literals_lambdas_and_comprehensions() {
    check(
        "def f(xs: list[int]):\n    a = [ 1,2 ]\n    b = {1:2,3:4}\n    c = {1,2}\n    d = ( 1, )\n    e = (a,b)=>a+b\n    [x*x for x in xs if x>0]\n",
        "def f(xs: list[int]):\n    a = [1, 2]\n    b = {1: 2, 3: 4}\n    c = {1, 2}\n    d = (1,)\n    e = (a, b) => a + b\n    [x * x for x in xs if x > 0]\n",
    );
}

#[test]
fn operator_methods_and_generics_are_written_tight() {
    check(
        "struct Grid[T :Shape]:\n    cells: list[T]\n\n    def + (self, other: Grid[T]):\n        self\n\n    def [] = (mut self, at: int, value: T):\n        pass\n",
        "struct Grid[T: Shape]:\n    cells: list[T]\n\n    def +(self, other: Grid[T]):\n        self\n\n    def []=(mut self, at: int, value: T):\n        pass\n",
    );
}

#[test]
fn match_arms_go_on_their_own_lines() {
    check(
        "def area(shape: Shape):\n    match shape :\n        case .Circle(radius):PI*radius*radius\n        case -1 : 0.0\n        case _:\n            0.0\n",
        "def area(shape: Shape):\n    match shape:\n        case .Circle(radius):\n            PI * radius * radius\n        case -1:\n            0.0\n        case _:\n            0.0\n",
    );
}

#[test]
fn soft_keywords_get_their_spaces() {
    check(
        "struct Score:\n    value: int where(value >= 0)\n",
        "struct Score:\n    value: int where (value >= 0)\n",
    );
    check(
        "def f(x: int):\n    match -x:\n        case(a, b): a\n",
        "def f(x: int):\n    match -x:\n        case (a, b):\n            a\n",
    );
}

#[test]
fn blocks_indent_four_spaces() {
    check(
        "def f(x: int):\n  if x > 0:\n          return 1\n  elif x < 0:\n   return -1\n  else:\n    return 0\n",
        "def f(x: int):\n    if x > 0:\n        return 1\n    elif x < 0:\n        return -1\n    else:\n        return 0\n",
    );
}

#[test]
fn declarations_get_one_blank_line_around_them() {
    check(
        "from std.math import Vec3\nfrom std.game import Id\ntype EnemyId = Id[Enemy]\nstruct A:\n    x: int\n    def get(self):\n        self.x\n    def set(mut self, x: int):\n        self.x = x\n\n\n\nerror Gone:\n    id: int\ndef main(stdout: Stdout):\n    pass\ntest \"it works\":\n    pass\n",
        "from std.game import Id\nfrom std.math import Vec3\ntype EnemyId = Id[Enemy]\n\nstruct A:\n    x: int\n\n    def get(self):\n        self.x\n\n    def set(mut self, x: int):\n        self.x = x\n\nerror Gone:\n    id: int\n\ndef main(stdout: Stdout):\n    pass\n\ntest \"it works\":\n    pass\n",
    );
}

#[test]
fn blank_lines_inside_blocks_are_kept_but_never_doubled() {
    check(
        "def f():\n\n\n    a = 1\n\n\n\n    b = 2\n    def inner():\n        pass\n    c = 3\n\n\n",
        "def f():\n    a = 1\n\n    b = 2\n    def inner():\n        pass\n    c = 3\n",
    );
}

#[test]
fn interfaces_keep_their_signatures_together() {
    check(
        "interface Shape:\n    def area(self) -> float\n    def name(self) -> str\n",
        "interface Shape:\n    def area(self) -> float\n    def name(self) -> str\n",
    );
}

#[test]
fn doc_comments_stay_with_what_they_document() {
    check(
        "## The module.\nimport std.game\n## A point.\nstruct Point:\n    ## Across.\n    x: int\n    ## Moves it.\n    def move(mut self):\n        pass\n## Runs it.\ndef main(stdout: Stdout):\n    pass\n",
        "## The module.\nimport std.game\n\n## A point.\nstruct Point:\n    ## Across.\n    x: int\n\n    ## Moves it.\n    def move(mut self):\n        pass\n\n## Runs it.\ndef main(stdout: Stdout):\n    pass\n",
    );
}

#[test]
fn comments_get_a_space_after_the_hash() {
    check(
        "#!odd   spacing  \n#no space\ndef f():  #   trailing   \n    pass\n",
        "#!odd   spacing\n# no space\ndef f():  #   trailing\n    pass\n",
    );
}

#[test]
fn trailing_comments_get_two_spaces_and_no_lining_up() {
    check(
        "def f():\n    x = 1 # one\n    longer = 2      # two\n\n    y = 3     # alone\n",
        "def f():\n    x = 1  # one\n    longer = 2  # two\n\n    y = 3  # alone\n",
    );
}

#[test]
fn own_line_comments_follow_their_column_between_blocks() {
    check(
        "def f(x: int):\n    if x:\n        a()\n        # still inside\n    # after the if\n    b()\n# after f\ndef g():\n    pass\n    # end of g\n# end of file\n",
        "def f(x: int):\n    if x:\n        a()\n        # still inside\n    # after the if\n    b()\n\n# after f\ndef g():\n    pass\n    # end of g\n# end of file\n",
    );
}

#[test]
fn long_lists_break_one_item_per_line_with_a_trailing_comma() {
    let long = "def f():\n    stdout.print(first_long_argument_name_here, second_long_argument_name_here, third_long_argument(1, 2, 3))\n";
    check(
        long,
        "def f():\n    stdout.print(\n        first_long_argument_name_here,\n        second_long_argument_name_here,\n        third_long_argument(1, 2, 3),\n    )\n",
    );
}

#[test]
fn breaking_starts_from_the_outside() {
    check(
        "def f():\n    outer(aaaaaaaaaaaaaaaaaaaaaaaaa, inner(bbbbbbbbbbbbbbbbbbbbbbbbbbbbb, ccccccccccccccccccccccccccccc, ddddddddddddddddddddddddd))\n",
        "def f():\n    outer(\n        aaaaaaaaaaaaaaaaaaaaaaaaa,\n        inner(\n            bbbbbbbbbbbbbbbbbbbbbbbbbbbbb,\n            ccccccccccccccccccccccccccccc,\n            ddddddddddddddddddddddddd,\n        ),\n    )\n",
    );
}

#[test]
fn long_signatures_break_their_parameters() {
    check(
        "def spawn_enemy(world: World, position: Vec3, velocity: Vec3, health: int, display_name: str) -> EnemyId?:\n    pass\n",
        "def spawn_enemy(\n    world: World,\n    position: Vec3,\n    velocity: Vec3,\n    health: int,\n    display_name: str,\n) -> EnemyId?:\n    pass\n",
    );
}

#[test]
fn lists_that_fit_join_even_with_a_trailing_comma() {
    check("def f():\n    g(\n        1,\n        2\n    )\n", "def f():\n    g(1, 2)\n");
    check("def f():\n    g(1, 2,)\n", "def f():\n    g(1, 2)\n");
    check("def f():\n    t = (\n        1,\n    )\n", "def f():\n    t = (1,)\n");
}

#[test]
fn no_comma_is_added_where_the_parser_wouldnt_take_one() {
    let long_name = "a".repeat(90);
    check(
        &format!("def f():\n    [{long_name} for x in xs]\n"),
        &format!("def f():\n    [\n        {long_name} for x in xs\n    ]\n"),
    );
    check(
        &format!("def f():\n    xs[{long_name}.count]\n"),
        &format!("def f():\n    xs[\n        {long_name}.count\n    ]\n"),
    );
    check(
        &format!("def f():\n    x = ({long_name}.count)\n"),
        &format!("def f():\n    x = (\n        {long_name}.count\n    )\n"),
    );
}

#[test]
fn comments_inside_brackets_keep_the_list_broken() {
    check(
        "def f():\n    xs = [  # numbers\n        1,  # one\n        2\n        # last\n    ]\n    g(a, # first\n      b)\n",
        "def f():\n    xs = [  # numbers\n        1,  # one\n        2,\n        # last\n    ]\n    g(\n        a,  # first\n        b,\n    )\n",
    );
}

#[test]
fn a_comment_inside_an_item_breaks_the_line_there() {
    check(
        "def f():\n    g(a +  # why\n      b)\n",
        "def f():\n    g(\n        a\n        +  # why\n        b,\n    )\n",
    );
}

#[test]
fn files_end_with_one_newline() {
    check("\n\n\ndef f():\n    pass", "def f():\n    pass\n");
    check("", "");
    check("# just a comment", "# just a comment\n");
}

#[test]
fn syntax_errors_are_reported_not_formatted() {
    assert!(matches!(fennec_fmt::format("def f(:\n"), Err(fennec_fmt::Error::Syntax(_))));
    assert!(matches!(fennec_fmt::format("def f():\n\tpass\n"), Err(fennec_fmt::Error::Syntax(_))));
}

#[test]
fn files_always_use_line_feeds() {
    check("def f():\r\n  pass\r\n", "def f():\n    pass\n");
}

#[test]
fn a_lone_list_argument_hugs_its_call() {
    check(
        "def f():\n    text = \"\\n\".join([\"first line of the file\", \"second line of the file\", \"the third line of the file\"])\n",
        "def f():\n    text = \"\\n\".join([\n        \"first line of the file\",\n        \"second line of the file\",\n        \"the third line of the file\",\n    ])\n",
    );
}

#[test]
fn a_body_too_long_for_its_line_moves_below_it() {
    check(
        "def f(x: int):\n    match x:\n        case 1: stdout.print(f\"the problem is on line {line}, column {column}, look there right now!\")\n        case _: pass\n",
        "def f(x: int):\n    match x:\n        case 1:\n            stdout.print(f\"the problem is on line {line}, column {column}, look there right now!\")\n        case _:\n            pass\n",
    );
}

#[test]
fn enum_case_defaults_keep_their_spaces() {
    check(
        "enum Move:\n    Wait(turns=1)\n    Step(dir: Dir, count: int=1)\n\ndef f():\n    Move.Wait(turns = 2)\n",
        "enum Move:\n    Wait(turns = 1)\n    Step(dir: Dir, count: int = 1)\n\ndef f():\n    Move.Wait(turns=2)\n",
    );
}

#[test]
fn guard_clauses_sit_on_one_line_and_other_bodies_dont() {
    check(
        "def f(x: int) -> int:\n    if x < 0:\n        return 0\n    if x > 9: fail TooBig(x)\n    if x == 5:\n        return 5\n    else:\n        return 6\n    if x == 3: return 3\n    elif x == 4: return 4\n    for i in range(3): print(i)\n    while True:\n        break\n    if x == 7:  # why\n        return 7\n    if x == 8:\n        # explain\n        return 8\n    x\n",
        "def f(x: int) -> int:\n    if x < 0: return 0\n    if x > 9: fail TooBig(x)\n    if x == 5:\n        return 5\n    else:\n        return 6\n    if x == 3:\n        return 3\n    elif x == 4:\n        return 4\n    for i in range(3):\n        print(i)\n    while True:\n        break\n    if x == 7:  # why\n        return 7\n    if x == 8:\n        # explain\n        return 8\n    x\n",
    );
}

#[test]
fn imports_are_sorted_with_std_first() {
    check(
        "## The module.\nfrom physics import Body, Air\nimport std.game\n\n# about math\nfrom std.math import Vec3, Mat4  # vectors\nfrom enemies import Orc\ndef main(stdout: Stdout):\n    pass\n",
        "## The module.\nimport std.game\n# about math\nfrom std.math import Mat4, Vec3  # vectors\n\nfrom enemies import Orc\nfrom physics import Air, Body\n\ndef main(stdout: Stdout):\n    pass\n",
    );
}

#[test]
fn long_statements_break_before_their_loosest_operators() {
    check(
        "def f(enemy: Enemy, world: World) -> int:\n    total_damage_dealt_this_frame = enemy.base_damage * enemy.level_multiplier + world.global_bonus_damage - enemy.armor\n    if enemy.health_points_remaining > 0 and enemy.is_visible_to_the_player and not world.paused_by_menu:\n        return enemy.base_damage * enemy.level_multiplier + world.global_bonus_damage - enemy.armor_value\n    x = [score for score in all_the_scores_in_this_world if score > minimum_score_needed_to_qualify_here]\n    0\n",
        "def f(enemy: Enemy, world: World) -> int:\n    total_damage_dealt_this_frame = (\n        enemy.base_damage * enemy.level_multiplier + world.global_bonus_damage - enemy.armor\n    )\n    if (\n        enemy.health_points_remaining > 0\n        and enemy.is_visible_to_the_player\n        and not world.paused_by_menu\n    ):\n        return (\n            enemy.base_damage * enemy.level_multiplier\n            + world.global_bonus_damage\n            - enemy.armor_value\n        )\n    x = [\n        score for score in all_the_scores_in_this_world if score > minimum_score_needed_to_qualify_here\n    ]\n    0\n",
    );
}

#[test]
fn lists_of_plain_literals_fill_their_lines() {
    check(
        "def f() -> list[float]:\n    m = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, -3.0, 4.25, 100.0]\n    names = [\"ash\", \"kit\", \"vex\", \"bramble\", \"fennec\", \"orc\", \"goblin\", \"troll\", \"dragon\", \"wyvern\", \"imp\"]\n    m\n",
        "def f() -> list[float]:\n    m = [\n        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, -3.0,\n        4.25, 100.0,\n    ]\n    names = [\n        \"ash\", \"kit\", \"vex\", \"bramble\", \"fennec\", \"orc\", \"goblin\", \"troll\", \"dragon\", \"wyvern\",\n        \"imp\",\n    ]\n    m\n",
    );
}
