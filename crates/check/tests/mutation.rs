//! Changing values: names have to be `mut`, `mut` is written at both ends of a call, and
//! methods that change a value need it in a `mut` place.

use crate::{helps, in_main, problems};

const ENEMY: &str = "
struct Enemy:
    health = 100
    tags = ['boss']

    def take_damage(ref self, amount: int):
        self.health -= amount

    def is_alive(self):
        self.health > 0

    def []=(ref self, at: int, value: int):
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

    def []=(ref self, at: int, value: Part):
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
            "mut slots = Slots(items=[])\nslots[0].x += 1.0\nslots[0] = Part()\nslots.total = 1\nstdout.print(slots.total)"
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

    def []=(ref self, at: int, value: int):
        self.cells[at] = value

struct Writes:
    cells: list[int]

    def []=(ref self, at: int, value: int):
        self.cells[at] = value
";
    assert_eq!(
        crate::in_main_after(types, "mut g = Grid(cells=[1])\ng[0] += 1\nstdout.print(g[0])"),
        Vec::<String>::new()
    );
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
    assert_eq!(
        in_main("mut count = 1\ncount += 1\nstdout.print(count)\ncount = 5\nstdout.print(count)"),
        Vec::<String>::new()
    );
    assert_eq!(in_main("a, b = (1, 2)\na, c = (3, 4)"), ["`a` isn't mut @ a"]);
}

#[test]
fn adding_mut_is_a_fix_to_make_by_hand() {
    let source = "def main(stdout: Stdout):\n    people = [\"Ash\"]\n    people = [\"Vex\"]\n";
    let module = fennec_syntax::parse(source).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{diagnostics:?}") };
    assert!(diagnostic.fixes.is_empty(), "`fennec fix` never adds `mut`");
    let hint = diagnostic.hint.as_ref().unwrap();
    assert_eq!(hint.says, "declare it `mut people`");
    let mut fixed = source.to_owned();
    for edit in &hint.edits {
        fixed.replace_range(edit.span.start as usize..edit.span.end as usize, &edit.text);
    }
    assert!(fixed.contains("    mut people = [\"Ash\"]\n"), "{fixed}");
    // `mut` can't be written into `a, b = ...`, so that one only says how.
    assert_eq!(
        helps("def main(stdout: Stdout):\n    a, b = (1, 2)\n    a = 3\n"),
        ["declare it with `mut a = ...` if it needs to change"]
    );
    assert_eq!(helps("def main(stdout: Stdout):\n    n: int = 1\n    n += 2\n"), ["fix: declare it `mut n`"]);
}

#[test]
fn the_fix_for_a_name_depends_on_where_it_came_from() {
    assert_eq!(helps("def main(stdout: Stdout):\n    x = 1\n    x = 2\n"), ["fix: declare it `mut x`"]);
    assert_eq!(
        helps("def heal(health: int):\n    health += 1\n"),
        [
            "parameters are read-only: to change it, write `ref health` in `heal`'s parameters, and pass it with `ref` at each call"
        ]
    );
    assert_eq!(
        helps("struct A:\n    n = 1\n\n    def bump(self):\n        self.n = 2\n"),
        ["to change `self`, write `ref self` in `bump`'s parameters"]
    );
    assert_eq!(
        helps("def f(items: list[int]):\n    for item in items:\n        item += 1\n"),
        ["loop with `for ref item in ...` to change each item"]
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
        mut enemies = Table[Enemy]()
        id = enemies.add(Enemy())
        _ = enemies.modify(id, enemy => enemy.take_damage(5))
    ";
    let declarations = format!("from std.collections import Table\n{ENEMY}");
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

/// `ref target = enemies[0]` names a field or an item of something held in a mut name, under a
/// name of its own: not a whole name, not something worked out, and not through itself.
#[test]
fn a_ref_names_a_field_or_item_of_a_mut_name() {
    let program = "
        enemies = [Enemy()]
        ref first = enemies[0]
        first.health = 1
        mut others = [Enemy()]
        ref other = others[0]
        other.health = 2
        ref whole = others
        ref made = Enemy()
        ref others = others[0]
    ";
    let a_place = "a `ref` names a field or an item, like `ref unit = units[at]`";
    assert_eq!(
        with_enemy(program),
        [
            "`enemies` isn't mut @ enemies[0]".to_owned(),
            format!("{a_place} @ others"),
            format!("{a_place} @ Enemy()"),
            "`others` is already the name this `ref` reaches through @ others".to_owned(),
        ]
    );
}

#[test]
fn methods_that_change_their_value_need_a_mut_place() {
    let program = "
        enemy = Enemy()
        enemy.take_damage(1)
        stdout.print(enemy.is_alive())
        mut hero = Enemy()
        hero.take_damage(1)
        _ = Enemy().take_damage(1)
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
        "pool.add(1)",
        "pool.clear()",
    ];
    for call in changing {
        // What a change gives back is thrown away on purpose, so only `mut` is in question.
        let program = format!("items = [1]\ntable = {{1: 2}}\nbag = {{1}}\npool = Table[int]()\n_ = {call}");
        let name = call.split('.').next().unwrap_or_default();
        let problems = crate::in_main_after("from std.collections import Table", &program);
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
        def heal(ref health: int, amount: int):
            health += amount

        def main(stdout: Stdout):
            mut health = 1
            heal(health, 1)
            heal(ref health, ref health)
            heal(ref health, 1)
            fixed = 1
            heal(ref fixed, 1)
            heal(ref 3, 1)
    ";
    assert_eq!(
        problems(program),
        [
            "`heal` changes `health`, so it has to be passed with `ref` @ health",
            "`heal` doesn't change `amount`, so it shouldn't be passed with `ref` @ health",
            "`fixed` isn't mut @ fixed",
            "only a name, or a field or item of one, can be passed with `ref` @ 3",
        ]
    );
    assert_eq!(
        helps("def f(ref n: int):\n    n += 1\n\ndef g():\n    mut n = 1\n    f(n)\n"),
        ["write `ref` before the argument, so readers can see it changes"]
    );
}

#[test]
fn for_mut_needs_a_mut_place() {
    let program = "
        def bump(ref scores: list[int], fixed: list[int]):
            for ref score in scores:
                score += 1
            for ref score in fixed:
                score += 1
            for ref n in [1, 2]:
                n += 1
            for score in scores:
                score += 1
    ";
    assert_eq!(
        problems(program),
        [
            "`fixed` isn't `ref` @ fixed",
            "`for ref` changes each item, so loop over something held in a mut name @ [1, 2]",
            "`score` isn't `ref` @ score",
        ]
    );
}

/// `for at, ref item in enumerate(items)` changes each item where it is, with its position: it
/// takes a place, as `for ref` does, and `ref` goes only before the second name, only with
/// `enumerate`.
#[test]
fn enumerate_with_ref_changes_each_item_where_it_is() {
    let program = "
        def bump(ref scores: list[int], fixed: list[int], ref more: dict[str, int]):
            for at, ref score in enumerate(scores):
                score += at
            for at, ref score in enumerate(fixed):
                score += at
            for at, ref one in enumerate(range(3)):
                one += at
            for key, ref value in more.items():
                pass
    ";
    assert_eq!(
        problems(program),
        [
            "`fixed` isn't `ref` @ fixed",
            "`for ref` changes each item, so loop over something held in a mut name @ range(3)",
            "`ref` with two names is for `enumerate(items)` @ more.items()",
        ]
    );
    let grows = "def bump(ref scores: list[int]):\n    for at, ref score in enumerate(scores):\n        scores.append(at)\n";
    assert_eq!(problems(grows), ["you can't change `scores` while looping over it @ scores.append(at)"]);
    let first =
        "def bump(ref scores: list[int]):\n    for ref at, score in enumerate(scores):\n        pass\n";
    assert_eq!(problems(first), ["parse: `ref` goes before the name whose item changes"]);
}

#[test]
fn with_names_can_be_mut() {
    let program = "
        with [1] as ref items:
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
        stdout.print(sorted(ref s), Enemy(ref n), 'a'.startswith(ref s), copy(Enemy(), ref n))
    ";
    assert_eq!(
        with_enemy(program),
        [
            "`sorted` doesn't change its arguments, so don't pass `ref` @ sorted(ref s)",
            "building a value doesn't change its arguments, so don't pass `ref` @ n",
            "`startswith` doesn't change `prefix`, so it shouldn't be passed with `ref` @ s",
            "`copy` doesn't change its arguments, so don't pass `ref` @ copy(Enemy(), ref n)",
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

#[test]
fn a_parallel_loop_changes_its_own_items() {
    let text = "struct Unit:\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units = [Unit(hp=1)]\n    \
        for ref unit in parallel(units):\n        unit.hp += 1\n        bonus = unit.hp * 2\n        stdout.print(bonus)\n";
    let empty: [&str; 0] = [];
    assert_eq!(problems(text), empty);
}

#[test]
fn a_parallel_loop_cant_change_names_outside_it() {
    let text = "def main(stdout: Stdout):\n    mut hps = [1, 2]\n    mut total = 0\n    mut log: list[int] = []\n    \
        for ref hp in parallel(hps):\n        total += hp\n        log.append(hp)\n    for hp in parallel(hps):\n        total = hp\n";
    assert_eq!(
        problems(text),
        [
            "a `parallel` loop can't change `total`, which is outside it @ total",
            "a `parallel` loop can't change `log`, which is outside it @ log",
            "a `parallel` loop can't change `total`, which is outside it @ total",
        ]
    );
}

#[test]
fn a_plain_name_read_from_a_place_says_where_to_write() {
    let types = "struct Stats:\n    mana = 1\n\nstruct Unit:\n    stats: Stats\n    health = 1\n\n    def heal(ref self):\n        self.health += 1\n";
    let start = "mut units = [Unit(stats=Stats())]\nat = 0\n";
    let help = |body: &str| {
        let lines = format!("{start}{body}").trim_end().replace('\n', "\n    ");
        helps(&format!("{types}\ndef main(stdout: Stdout):\n    {lines}\n"))
    };
    for (body, said) in [
        ("target = units[at]\ntarget.health -= 10\n", "to change `units`: write to `units[at].health`, or name the place with `ref target = units[at]`"),
        ("target = units[0]\ntarget.stats.mana = 2\n", "to change `units`: write to `units[0].stats.mana`, or name the place with `ref target = units[0]`"),
        ("target = units[at]\ntarget.heal()\n", "to change `units`: write to `units[at]`, or name the place with `ref target = units[at]`"),
        ("stats = units[at].stats\nstats.mana += 1\n", "to change `units`: write to `units[at].stats.mana`, or name the place with `ref stats = units[at].stats`"),
    ] {
        assert_eq!(help(body), [said], "{body}");
    }
    assert_eq!(help("plain = Stats()\nplain.mana = 2\n"), ["fix: declare it `mut plain`"]);
}

#[test]
fn a_name_that_hides_a_mut_one_says_so() {
    let said = "this `k` is a new name, which hides the `mut k` around it @ k";
    let types = "enum Sized:\n    Small(size: int)\n";
    for body in [
        "mut k = 0\nmatch Sized.Small(size=1):\n    case .Small(k):\n        k += 1\nstdout.print(k)\n",
        "mut k = 0\nfor k in range(3):\n    k += 1\nstdout.print(k)\n",
    ] {
        assert_eq!(crate::in_main_after(types, body), [said], "{body}");
    }
    assert_eq!(crate::in_main_after(types, "k = 0\nk += 1\nstdout.print(k)\n"), ["`k` isn't mut @ k"]);
}

#[test]
fn a_generator_cant_change_whats_around_it() {
    let empty: [&str; 0] = [];
    let bump = "def bump(ref k: int) -> int:\n    k += 1\n    k\n";
    let told = |name: &str, at: &str| {
        vec![format!("a generator kept for later can't change `{name}`, which is defined outside it @ {at}")]
    };
    let kept = "mut k = 1\ng = (bump(ref k) for x in range(3))\nstdout.print(list(g), k)\n";
    assert_eq!(crate::in_main_after(bump, kept), told("k", "k"));
    let method = "mut xs = [1, 2]\ng = (xs.pop() for x in range(2))\nstdout.print(list(g), xs)\n";
    assert_eq!(crate::in_main_after(bump, method), told("xs", "xs"));
    let looped =
        "mut k = 1\nfor one in (bump(ref k) for x in range(3)):\n    stdout.print(one)\nstdout.print(k)\n";
    assert_eq!(crate::in_main_after(bump, looped), told("k", "k"));
    // Written right inside what uses it, it only reads too.
    let around = |at: &str| vec![format!("this generator changes `k`, a name around it @ {at}")];
    for used in ["sum", "min", "max", "sorted", "list", "set"] {
        let body = format!("mut k = 1\nstdout.print({used}(bump(ref k) for x in range(3)), k)\n");
        assert_eq!(crate::in_main_after(bump, &body), around("bump(ref k)"), "{body}");
    }
    let asked = "mut k = 1\nstdout.print(any(bump(ref k) > 9 for x in range(3)), k)\n";
    assert_eq!(crate::in_main_after(bump, asked), around("bump(ref k)"));
    let method = "mut xs = [1, 2]\nstdout.print(sum(xs.pop() else 0 for x in range(2)), xs)\n";
    assert_eq!(
        crate::in_main_after(bump, method),
        ["this generator changes `xs`, a name around it @ xs.pop()"]
    );
    let own = "xs = [[1], [2]]\nstdout.print(sum(inner.len() for inner in xs))\n";
    assert_eq!(crate::in_main_after(bump, own), empty);
    let read = "k = 1\ng = (k + x for x in range(3))\nstdout.print(list(g))\n";
    assert_eq!(crate::in_main_after(bump, read), empty);
}

#[test]
fn a_parallel_loop_can_change_a_shared_value_outside_it() {
    let source = "struct Counter:\n    total = 0\n\n    def add(ref self, amount: int):\n        self.total += amount\n\n\
                  def main(stdout: Stdout):\n    shared counter = Counter()\n    mut hps = [1, 2]\n    \
                  for ref hp in parallel(hps):\n        lock counter as ref held:\n            held.add(hp)\n            \
                  held.total += hp\n        hp += 1\n    lock counter as seen:\n        stdout.print(seen.total, hps)\n";
    assert_eq!(problems(source), Vec::<String>::new());
}

/// `case .Button(ref clicks):` names the field itself: the `match` is a statement over a place
/// in a mut name, giving back nothing, and the `ref` is right inside a case's parentheses, beside
/// names, `_`s and literals.
#[test]
fn a_ref_case_changes_a_field_of_a_place() {
    let program = |body: &str| {
        problems(&format!(
            "enum Kind:\n    Button(clicks: int, label: str)\n    Empty\n\n\
             struct Pair:\n    first: Kind\n    second: Kind\n\n\
             def make() -> Kind:\n    .Empty\n\n\
             def main(stdout: Stdout):\n    mut kind = Kind.Button(clicks=0, label='a')\n    plain = Kind.Empty\n{body}"
        ))
    };
    let empty: [&str; 0] = [];
    assert_eq!(
        program(
            "    match kind:\n        case .Button(ref clicks, _):\n            clicks += 1\n        case _:\n            pass\n"
        ),
        empty
    );
    assert_eq!(
        program(
            "    match plain:\n        case .Button(ref clicks, _):\n            clicks += 1\n        case _:\n            pass\n"
        ),
        ["`plain` isn't mut @ plain"]
    );
    assert_eq!(
        program(
            "    match make():\n        case .Button(ref clicks, _):\n            clicks += 1\n        case _:\n            pass\n"
        ),
        ["a `ref` case changes what the `match` is over, which has to be a place @ make()"]
    );
    let found = program(
        "    count = match kind:\n        case .Button(ref clicks, _): clicks\n        case _: 0\n    stdout.print(count)\n",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("a `match` with a `ref` case is a statement of its own"), "{found:?}");
    let found = program(
        "    stdout.print(lasts(ref kind))\n\ndef lasts(ref kind: Kind) -> int:\n    match kind:\n        case .Button(ref clicks, _): clicks\n        case _: 0\n",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("a `match` with a `ref` case gives back nothing"), "{found:?}");
}

/// `embed` is now `embed_file`, and `fennec fix` renames it.
#[test]
fn embed_is_renamed_to_embed_file() {
    let module =
        fennec_syntax::parse("def main(stdout: Stdout):\n    stdout.print(embed(\"data.bin\").len())\n")
            .unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{diagnostics:?}") };
    assert_eq!(diagnostic.message, "`embed` doesn't exist");
    let fixed: Vec<&str> = diagnostic.fixes.iter().map(|fix| fix.text.as_str()).collect();
    assert_eq!(fixed, ["embed_file"]);
}
