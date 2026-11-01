//! Changing values: names have to be `mut`, `mut` is written at both ends of a call, and
//! methods that change a value need it in a `mut` place.

use crate::{helps, in_main, problems};

const ENEMY: &str = "
struct Enemy:
    health = 100
    tags = ['boss']

    def take_damage(mut self, amount: int):
        self.health -= amount

    def is_alive(self):
        self.health > 0

    def []=(mut self, at: int, value: int):
        self.health = value
";

fn with_enemy(body: &str) -> Vec<String> {
    crate::in_main_after(ENEMY, body)
}

#[test]
fn a_types_own_index_needs_a_write_to_change_an_item() {
    let types = "
struct Part:
    x = 0.0

struct Parts:
    items: list[Part]

    def [](self, at: int) -> Part:
        self.items[at]

struct Slots:
    items: list[Part]
    total = 0

    def [](self, at: int) -> Part:
        self.items[at]

    def []=(mut self, at: int, value: Part):
        self.items[at] = value
";
    let only_read = "`Parts` has a `def []` but no `def []=`, so its items can't be changed";
    assert_eq!(
        crate::in_main_after(types, "mut parts = Parts(items=[])\nparts[0].x = 1.0"),
        [format!("{only_read} @ parts[0]")]
    );
    assert_eq!(
        crate::in_main_after(types, "mut parts = Parts(items=[])\nparts[0] = Part()"),
        [format!("{only_read} @ parts[0]")]
    );
    assert_eq!(
        crate::in_main_after(
            types,
            "mut slots = Slots(items=[])\nslots[0].x += 1.0\nslots[0] = Part()\nslots.total = 1"
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        crate::in_main_after(types, "slots = Slots(items=[])\nslots[0].x = 1.0"),
        ["`slots` isn't mut @ slots[0].x"]
    );
}

#[test]
fn compound_assignment_through_a_types_own_index_uses_its_operator() {
    let types = "
struct Grid:
    cells: list[int]

    def [](self, at: int) -> int:
        self.cells[at]

    def []=(mut self, at: int, value: int):
        self.cells[at] = value

struct Writes:
    cells: list[int]

    def []=(mut self, at: int, value: int):
        self.cells[at] = value
";
    assert_eq!(crate::in_main_after(types, "mut g = Grid(cells=[1])\ng[0] += 1"), Vec::<String>::new());
    assert_eq!(
        crate::in_main_after(types, "mut g = Grid(cells=[1])\ng[0] += 1.5"),
        ["can't mix int and float in `+` @ g[0]"]
    );
    assert_eq!(crate::in_main_after(types, "g = Grid(cells=[1])\ng[0] += 1"), ["`g` isn't mut @ g[0]"]);
    assert_eq!(
        crate::in_main_after(types, "mut w = Writes(cells=[1])\nw[0] += 1"),
        ["`Writes` can't be indexed @ w[0]"]
    );
}

#[test]
fn names_change_only_when_mut() {
    assert_eq!(in_main("name = 'Ash'\nname = 'Vex'"), ["`name` isn't mut @ name"]);
    assert_eq!(in_main("count = 1\ncount += 1"), ["`count` isn't mut @ count"]);
    assert_eq!(in_main("mut count = 1\ncount += 1\ncount = 5"), Vec::<String>::new());
    assert_eq!(in_main("a, b = (1, 2)\na, c = (3, 4)"), ["`a` isn't mut @ a"]);
}

#[test]
fn the_fix_for_a_name_depends_on_where_it_came_from() {
    assert_eq!(
        helps("def main(stdout: Stdout):\n    x = 1\n    x = 2\n"),
        ["declare it with `mut x = ...` if it needs to change"]
    );
    assert_eq!(
        helps("def heal(health: int):\n    health += 1\n"),
        [
            "parameters are read-only: to change it, write `mut health` in `heal`'s parameters, and pass it with `mut` at each call"
        ]
    );
    assert_eq!(
        helps("struct A:\n    n = 1\n\n    def bump(self):\n        self.n = 2\n"),
        ["to change `self`, write `mut self` in `bump`'s parameters"]
    );
    assert_eq!(
        helps("def f(items: list[int]):\n    for item in items:\n        item += 1\n"),
        ["loop with `for mut item in ...` to change each item"]
    );
}

#[test]
fn functions_cannot_change_the_names_around_them() {
    let program = "
        mut seen = []
        mut total = 0
        def add(n: int):
            total += n
        stdout.print(sorted([1], key=n => seen.append(n)))
    ";
    assert_eq!(
        in_main(program),
        [
            "a nested function can't change `total`, which is defined outside it @ total",
            "an arrow function can't change `seen`, which is defined outside it @ seen",
        ]
    );
    let program = "
        def f(items: list[int]):
            mut seen: list[int] = []
            def g(n: int):
                mut inner = [n]
                inner.append(n)
                inner
            [g(item) for item in items]
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn arrow_function_parameters_can_change() {
    let program = "
        mut enemies = IdMap[Enemy]()
        id = enemies.insert(Enemy())
        enemies.modify(id, enemy => enemy.take_damage(5))
    ";
    let declarations = format!("from std.game import IdMap\n{ENEMY}");
    assert_eq!(crate::in_main_after(&declarations, program), Vec::<String>::new());
}

#[test]
fn declaring_a_name_twice_is_a_mistake() {
    assert_eq!(in_main("x = 1\nmut x = 2"), ["`x` already exists @ x"]);
    assert_eq!(
        helps("def main(stdout: Stdout):\n    mut x = 1\n    mut x = 2\n"),
        ["to change it, write `x = ...` without `mut`"]
    );
    assert_eq!(in_main("total += 1"), ["`total` doesn't exist yet @ total"]);
}

#[test]
fn fields_and_items_change_only_through_mut_names() {
    let program = "
        enemy = Enemy()
        enemy.health = 5
        mut other = Enemy()
        other.health = 5
        other.health += 1
        scores = [1]
        scores[0] = 2
        mut grid = [0]
        grid[0] += 1
    ";
    assert_eq!(with_enemy(program), ["`enemy` isn't mut @ enemy.health", "`scores` isn't mut @ scores[0]"]);
}

#[test]
fn methods_that_change_their_value_need_a_mut_place() {
    let program = "
        enemy = Enemy()
        enemy.take_damage(1)
        stdout.print(enemy.is_alive())
        mut hero = Enemy()
        hero.take_damage(1)
        Enemy().take_damage(1)
        enemy[0] = 5
        hero[0] = 5
    ";
    assert_eq!(
        with_enemy(program),
        [
            "`enemy` isn't mut @ enemy",
            "`take_damage` changes its value, so call it on something held in a mut name @ Enemy()",
            "`enemy` isn't mut @ enemy",
        ]
    );
}

#[test]
fn builtin_methods_that_change_a_value_need_a_mut_place() {
    let changing = [
        "items.append(1)",
        "items.extend([1])",
        "items.insert(0, 1)",
        "items.clear()",
        "items.reverse()",
        "items.remove(1)",
        "items.remove_where(n => n > 1)",
        "items.sort()",
        "items.pop()",
        "table.pop(1)",
        "table.clear()",
        "bag.add(1)",
        "bag.discard(1)",
        "bag.remove(1)",
        "bag.clear()",
        "pool.insert(1)",
        "pool.clear()",
    ];
    for call in changing {
        let program = format!("items = [1]\ntable = {{1: 2}}\nbag = {{1}}\npool = IdMap[int]()\n{call}");
        let name = call.split('.').next().unwrap_or_default();
        let problems = crate::in_main_after("from std.game import IdMap", &program);
        assert_eq!(problems, [format!("`{name}` isn't mut @ {name}")], "{call}");
    }
    let reading = "items = [1]\nstdout.print(items.index(1), items.count(1), 'a'.upper())";
    assert_eq!(in_main(reading), Vec::<String>::new());
    assert_eq!(
        in_main("[1, 2].append(3)"),
        [
            "`append` changes it, so call it on something held in a mut name, like `items.append(...)` @ [1, 2]"
        ]
    );
}

#[test]
fn mut_arguments_match_mut_parameters() {
    let program = "
        def heal(mut health: int, amount: int):
            health += amount

        def main(stdout: Stdout):
            mut health = 1
            heal(health, 1)
            heal(mut health, mut health)
            heal(mut health, 1)
            fixed = 1
            heal(mut fixed, 1)
            heal(mut 3, 1)
    ";
    assert_eq!(
        problems(program),
        [
            "`heal` changes `health`, so it has to be passed with `mut` @ health",
            "`heal` doesn't change `amount`, so it shouldn't be passed with `mut` @ health",
            "`fixed` isn't mut @ fixed",
            "only a name, or a field or item of one, can be passed with `mut` @ 3",
        ]
    );
    assert_eq!(
        helps("def f(mut n: int):\n    n += 1\n\ndef g():\n    mut n = 1\n    f(n)\n"),
        ["write `mut` before the argument, so readers can see it changes"]
    );
}

#[test]
fn for_mut_needs_a_mut_place() {
    let program = "
        def bump(mut scores: list[int], fixed: list[int]):
            for mut score in scores:
                score += 1
            for mut score in fixed:
                score += 1
            for mut n in [1, 2]:
                n += 1
            for score in scores:
                score += 1
    ";
    assert_eq!(
        problems(program),
        [
            "`fixed` isn't mut @ fixed",
            "`for mut` changes each item, so loop over something held in a mut name @ [1, 2]",
            "`score` isn't mut @ score",
        ]
    );
}

#[test]
fn with_names_can_be_mut() {
    let program = "
        with [1] as mut items:
            items.append(2)
        with [1] as items:
            items.append(2)
    ";
    assert_eq!(in_main(program), ["`items` isn't mut @ items"]);
}

#[test]
fn values_and_builtins_do_not_take_mut() {
    let program = "
        mut n = 1
        mut s = 'a'
        stdout.print(sorted(mut s), Enemy(mut n), 'a'.startswith(mut s), replace(Enemy(), mut n))
    ";
    assert_eq!(
        with_enemy(program),
        [
            "`sorted` doesn't change its arguments, so don't pass `mut` @ sorted(mut s)",
            "building a value doesn't change its arguments, so don't pass `mut` @ n",
            "`startswith` doesn't change `prefix`, so it shouldn't be passed with `mut` @ s",
            "`replace` doesn't change its arguments, so don't pass `mut` @ replace(Enemy(), mut n)",
            "name the field to change, like `health=50` @ n",
        ]
    );
}

#[test]
fn only_places_can_be_assigned() {
    let program = "
        def make():
            [1]

        def main(stdout: Stdout):
            make()[0] = 1
    ";
    assert_eq!(problems(program), ["this can't be assigned to @ make()[0]"]);
}
