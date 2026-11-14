//! A collection can't change while a loop goes over it, in the loop's body or in anything the
//! body calls.

use crate::{helps, in_main_after, problems};

const TEAM: &str = "struct Team:
    names = list[str]()
    wins = 0

    def add(ref self, name: str):
        self.names.append(name)

    def win(ref self):
        self.wins += 1

def grow(ref names: list[str]):
    names.append(\"x\")
";

fn changes(body: &str) -> Vec<String> {
    in_main_after(
        TEAM,
        &format!(
            "mut names = [\"ash\", \"kit\"]\nmut team = Team()\nmut scores = {{\"a\": 1}}\n{body}stdout.print(names, team, scores)\n"
        ),
    )
}

#[test]
fn a_loop_can_change_what_it_doesnt_go_over() {
    let empty: [&str; 0] = [];
    assert_eq!(changes("for name in team.names:\n    team.win()\n"), empty);
    assert_eq!(changes("before = names\nfor name in before:\n    names.append(name)\n"), empty);
    assert_eq!(changes("for name in names[1:]:\n    names.append(name)\n"), empty);
    assert_eq!(
        changes("mut found = list[str]()\nfor name in names:\n    found.append(name)\nnames = found\n"),
        empty
    );
    assert_eq!(changes("for ref name in names:\n    name = name.upper()\n"), empty);
}

/// `for key, ref value in x.items()` goes over a dict or a `Table` held in a `mut` name, its
/// value changed where it is and its key as it is: the body can't change `x` itself, the key isn't
/// `ref`, and a list's positions are `enumerate`'s.
#[test]
fn a_ref_over_items_changes_only_the_values() {
    let program = "
        def main(stdout: Stdout):
            mut scores = {'x': 1}
            for key, ref score in scores.items():
                score += len(key)
            stdout.print(scores)
    ";
    assert_eq!(problems(program), Vec::<String>::new());
    let refused = "
        def main(stdout: Stdout):
            mut scores = {'x': 1}
            for key, ref score in scores.items():
                key += '!'
            kept = {'x': 1}
            for key, ref score in kept.items():
                score += 1
            mut xs = [1]
            for at, ref x in xs.items():
                x += 1
            stdout.print(scores, kept, xs)
    ";
    assert_eq!(
        problems(refused),
        [
            "`key` isn't `ref` @ key",
            "`kept` isn't mut @ kept",
            "`for key, ref value in x.items()` goes over a dict or a `Table`, but this is a list[int] @ xs",
        ]
    );
}

#[test]
fn a_loop_cant_change_what_it_goes_over() {
    let changed =
        |shown: &str, at: &str| vec![format!("you can't change `{shown}` while looping over it @ {at}")];
    assert_eq!(
        changes("for name in names:\n    names.append(name)\n"),
        changed("names", "names.append(name)")
    );
    assert_eq!(
        changes("for at, name in enumerate(names):\n    names[at] = name.upper()\n"),
        changed("names", "names[at]")
    );
    assert_eq!(changes("for name in names:\n    grow(ref names)\n"), changed("names", "names"));
    assert_eq!(
        changes("for name in team.names:\n    team.add(name)\n"),
        changed("team.names", "team.add(name)")
    );
    assert_eq!(
        changes("for key in scores.keys():\n    _ = scores.pop(key)\n"),
        changed("scores", "scores.pop(key)")
    );
    assert_eq!(
        changes(
            "for name in names:\n    if name == \"ash\":\n        for other in names:\n            names.remove(other)\n"
        ),
        [
            "you can't change `names` while looping over it @ names.remove(other)",
            "you can't change `names` while looping over it @ names.remove(other)",
        ]
    );
    assert_eq!(
        helps(&format!(
            "{TEAM}\ndef main(stdout: Stdout):\n    mut names = [\"ash\"]\n    for name in names:\n        names.append(name)\n"
        )),
        ["use `for ref`, or loop over indexes"]
    );
}

/// `ref first = names[0]` has its place to itself from the `ref` to the last line that uses it:
/// nothing else may change what it's in, and it's one `ref` into it at a time. Reads are fine,
/// and so is a change once its last use is past, or to another field.
#[test]
fn a_local_ref_has_its_place_to_itself_while_its_in_use() {
    let empty: [&str; 0] = [];
    assert_eq!(changes("ref first = names[0]\nfirst = first.upper()\nnames.append(\"z\")\n"), empty);
    assert_eq!(changes("ref first = names[0]\nfirst += names[1]\n"), empty);
    assert_eq!(changes("ref wins = team.wins\nteam.names.append(\"z\")\nwins += 1\n"), empty);
    assert_eq!(
        changes("ref first = names[0]\nnames.append(\"z\")\nfirst += \"!\"\n"),
        vec!["this changes `names` while `first` is a `ref` to `names[0]` @ names.append(\"z\")"]
    );
    assert_eq!(
        changes("ref first = names[0]\nif first == \"ash\":\n    grow(ref names)\nfirst += \"!\"\n"),
        vec!["this changes `names` while `first` is a `ref` to `names[0]` @ names"]
    );
    assert_eq!(
        changes("ref first = names[0]\nref second = names[1]\nfirst += second\n"),
        vec!["`second` is a second `ref` into `names[...]` while `first` is still in use @ names[1]"]
    );
    assert_eq!(
        changes("for name in names:\n    ref first = names[0]\n    first += name\n"),
        vec!["you can't change `names` while looping over it @ names[0]"]
    );
}

#[test]
fn writing_through_a_collection_changes_it_for_a_loop_over_it() {
    let problem = ["you can't change `names` while looping over it @ names"];
    for body in [
        "for ref name in names:\n    for ref other in names:\n        other += name\n",
        "for name in names:\n    for ref other in names:\n        other += name\n",
        "for ref name in names:\n    if name == \"a\":\n        for ref other in names:\n            other += name\n",
    ] {
        assert_eq!(changes(body), problem, "{body}");
    }
}

#[test]
fn a_function_that_writes_through_a_collection_changes_it() {
    let writes = "def shout(ref items: list[str]):\n    for ref item in items:\n        item = item.upper()\n\n\
                  def twice(ref items: list[str]):\n    shout(ref items)\n\n";
    let problem = ["you can't change `names` while looping over it @ names"];
    for call in ["shout", "twice"] {
        let body = format!("mut names = [\"a\"]\nfor name in names:\n    {call}(ref names)\n");
        assert_eq!(in_main_after(writes, &body), problem, "{call}");
    }
}

#[test]
fn a_nested_for_mut_over_something_else_is_fine() {
    let empty: [&str; 0] = [];
    let body =
        "mut grid = [[1, 2], [3]]\nfor ref row in grid:\n    for ref cell in row:\n        cell += 1\n";
    assert_eq!(in_main_after("", body), empty);
    let body = "mut xs = [1]\nmut ys = [2]\nfor ref x in xs:\n    for ref y in ys:\n        y += x\n";
    assert_eq!(in_main_after("", body), empty);
    let body = "mut xs = [1]\nfor ref x in xs:\n    x += 1\nfor ref x in xs:\n    x += 1\n";
    assert_eq!(in_main_after("", body), empty);
}

#[test]
fn a_for_mut_over_an_id_map_locks_it_too() {
    let body = "mut units = Table[int]()\nfor ref unit in units:\n    for ref other in units:\n        other += unit\n";
    assert_eq!(
        in_main_after("from std.collections import Table\n\n", body),
        ["you can't change `units` while looping over it @ units"]
    );
}

#[test]
fn a_for_mut_isnt_told_to_loop_over_a_copy() {
    let help = helps(
        "def main(stdout: Stdout):\n    mut xs = [1]\n    for ref x in xs:\n        for ref y in xs:\n            y += x\n",
    );
    assert_eq!(help, ["loop over indexes, or collect the changes and make them after the loop"]);
}

const PLACES: &str = "struct Spot:
    x = 0

struct Unit:
    position = Spot()
    home = Spot()
    spots = list[Spot]()

    def absorb(ref self, ref other: Unit):
        self.position.x += other.position.x

    def look(self, ref at: Spot):
        at.x = self.position.x

    def keep(ref self, spot: Spot):
        self.spots.append(spot)

def bump(ref first: Spot, ref second: Spot):
    first.x += 1
    second.x += 1

def read(ref first: Spot, second: Spot):
    first.x = second.x

def fill(ref spots: list[Spot], spot: Spot):
    spots.append(spot)

def more(ref spots: list[Spot], count: int):
    spots.append(Spot(x=count))
";

fn given(body: &str) -> Vec<String> {
    in_main_after(
        PLACES,
        &format!(
            "mut unit = Unit()\nmut units = [Unit()]\nmut spots = [Spot()]\nmut i = 0\nmut j = 0\n{body}"
        ),
    )
}

#[test]
fn one_place_cant_be_given_as_mut_with_anything_that_reaches_it() {
    let cases: [(&str, &str); 8] = [
        ("bump(ref unit.position, ref unit.position)", "`ref unit.position` and `ref unit.position` overlap"),
        ("bump(ref spots[i], ref spots[j])", "`ref spots[i]` and `ref spots[j]` overlap"),
        ("bump(ref spots[0], ref spots[0])", "`ref spots[0]` and `ref spots[0]` overlap"),
        ("read(ref unit.position, unit.position)", "`ref unit.position` and `unit.position` overlap"),
        ("fill(ref spots, spots[i])", "`ref spots` and `spots[i]` overlap"),
        ("fill(ref unit.spots, unit.spots[i + 1])", "`ref unit.spots` and `unit.spots[...]` overlap"),
        ("unit.absorb(ref unit)", "`ref unit` overlaps `unit`, which this method is called on"),
        (
            "unit.look(ref unit.position)",
            "`ref unit.position` overlaps `unit`, which this method is called on",
        ),
    ];
    for (call, said) in cases {
        let found = given(&format!("{call}\nstdout.print(unit.position.x, len(spots))\n"));
        let messages: Vec<&str> = found.iter().filter_map(|one| one.split(" @ ").next()).collect();
        assert_eq!(messages, [said], "{call}");
    }
}

#[test]
fn a_ref_self_method_needs_its_value_to_itself() {
    let types = "struct Bag:\n    name = \"\"\n    size = 0\n    items = list[int]()\n\n    def absorb(ref self, other: Bag):\n        self.size += other.size\n\n    def grab(ref self, more: list[int]):\n        self.items.append(len(more))\n\n    def rename(ref self, name: str):\n        self.name = name\n\n    def grow(ref self, by: int):\n        self.size += by\n\n    def has(self, other: Bag) -> bool:\n        return self.size == other.size\n";
    let cases: [(&str, &[&str]); 8] = [
        ("bag.absorb(bag)", &["`bag` overlaps `bag`, which this method changes"]),
        ("bag.grab(bag.items)", &["`bag.items` overlaps `bag`, which this method changes"]),
        ("bag.rename(bag.name)", &["`bag.name` overlaps `bag`, which this method changes"]),
        ("bags[i].absorb(bags[j])", &["`bags[j]` overlaps `bags[i]`, which this method changes"]),
        ("bag.grow(bag.size)", &[]),
        ("held = bag\nbag.absorb(held)", &[]),
        ("bags[0].absorb(bags[1])", &[]),
        ("stdout.print(bag.has(bag))", &[]),
    ];
    for (call, said) in cases {
        let body = format!(
            "mut bag = Bag()\nmut bags = [Bag(), Bag()]\ni = 0\nj = 1\n{call}\nstdout.print(bag.size, len(bags), i, j)\n"
        );
        let found = in_main_after(types, &body);
        let messages: Vec<&str> = found.iter().filter_map(|one| one.split(" @ ").next()).collect();
        assert_eq!(messages, said, "{call}");
    }
}

#[test]
fn a_mut_argument_cant_change_what_picks_another_arguments_place() {
    let spot = "def step(ref at: int, spot: Spot):\n    at += spot.x\n\ndef walk(ref spot: Spot, ref at: int):\n    at += spot.x\n\n";
    let types = format!("{PLACES}\n{spot}");
    let cases: [(&str, &str); 3] = [
        ("step(ref i, spots[i])", "`ref i` changes what picks `spots[i]`"),
        ("walk(ref spots[i], ref i)", "`ref i` changes what picks `spots[i]`"),
        ("walk(ref spots[i + 1], ref i)", "`ref i` changes what picks `spots[...]`"),
    ];
    for (call, said) in cases {
        let body = format!(
            "mut unit = Unit()\nmut units = [Unit()]\nmut spots = [Spot()]\nmut i = 0\nmut j = 0\n{call}\nstdout.print(unit.position.x, len(spots), len(units), i, j)\n"
        );
        let found = in_main_after(&types, &body);
        let messages: Vec<&str> = found.iter().filter_map(|one| one.split(" @ ").next()).collect();
        assert_eq!(messages, [said], "{call}");
    }
    let moved = "struct Mover:\n    x = 0\n\n    def shift(ref self, ref at: int):\n        at += self.x\n";
    let found = in_main_after(
        moved,
        "mut movers = [Mover()]\nmut at = 0\nmovers[at].shift(ref at)\nstdout.print(len(movers), at)\n",
    );
    let messages: Vec<&str> = found.iter().filter_map(|one| one.split(" @ ").next()).collect();
    assert_eq!(messages, ["`ref at` changes what picks `movers[at]`"]);
    // An index that isn't changed is fine, and so is a name changed that picks nothing here.
    let empty: [&str; 0] = [];
    let fine = "mut movers = [Mover()]\nmut at = 0\nmut other = 0\nmovers[at].shift(ref other)\nstdout.print(len(movers), at, other)\n";
    assert_eq!(in_main_after(moved, fine), empty);
}

/// A pool whose methods change one field each, for an argument changing what its call changes.
const POOL: &str = "def push(ref items: list[int], value: int):
    items.append(value)

struct Pool:
    items = list[int]()
    spare = list[int]()

    def grow(ref self) -> int:
        self.items.append(9)
        return len(self.items)

    def take(ref self) -> int:
        self.spare.append(1)
        return len(self.spare)

struct Holder:
    pool = Pool()
";

#[test]
fn an_argument_cant_change_what_its_call_changes() {
    let said = "this changes `holder.pool.items`, which the call it's passed to changes too";
    let cases = [
        ("holder.pool.items.append(holder.pool.grow())", said, "holder.pool.grow()"),
        ("push(ref holder.pool.items, holder.pool.grow())", said, "holder.pool.grow()"),
        ("holder.pool.items.append(holder.pool.items.pop() else 0)", said, "holder.pool.items.pop()"),
    ];
    for (call, message, at) in cases {
        let body = format!("mut holder = Holder()\n{call}\nstdout.print(holder.pool.items)\n");
        assert_eq!(in_main_after(POOL, &body), [format!("{message} @ {at}")], "{call}");
    }
    let empty: [&str; 0] = [];
    for call in [
        "holder.pool.items.append(len(holder.pool.items))",
        "holder.pool.items.append(holder.pool.take())",
        "push(ref holder.pool.items, holder.pool.take())",
        "holder.pool.spare.append(holder.pool.grow())",
        "mut lists = [[1], [2]]\nlists[0].append(lists[1].pop() else 0)\nstdout.print(lists)",
    ] {
        let body = format!("mut holder = Holder()\n{call}\nstdout.print(holder.pool.items)\n");
        assert_eq!(in_main_after(POOL, &body), empty, "{call}");
    }
}

/// Two `ref`s into one collection overlap however they're written, `spots[0]` and `spots[1]` too:
/// each has its place to itself.
#[test]
fn two_refs_into_one_collection_overlap_however_theyre_written() {
    for (call, said) in [
        ("bump(ref spots[0], ref spots[1])", "`ref spots[0]` and `ref spots[1]` overlap"),
        (
            "bump(ref units[0].spots[1], ref units[0].spots[2])",
            "`ref units[0].spots[1]` and `ref units[0].spots[2]` overlap",
        ),
        (
            "bump(ref units[0].position, ref units[1].position)",
            "`ref units[0].position` and `ref units[1].position` overlap",
        ),
        ("bump(ref spots[0], ref spots[-1])", "`ref spots[0]` and `ref spots[...]` overlap"),
        ("bump(ref spots[0], ref spots[i])", "`ref spots[0]` and `ref spots[i]` overlap"),
        ("bump(ref spots[i], ref spots[i + 1])", "`ref spots[i]` and `ref spots[...]` overlap"),
    ] {
        let found = given(&format!("{call}\nstdout.print(unit.position.x, len(spots))\n"));
        let messages: Vec<&str> = found.iter().filter_map(|one| one.split(" @ ").next()).collect();
        assert_eq!(messages, [said], "{call}");
    }
}

#[test]
fn different_places_can_be_given_as_mut_together() {
    let empty: [&str; 0] = [];
    for call in [
        "bump(ref unit.position, ref unit.home)",
        "bump(ref units[i].position, ref units[j].home)",
        // A value read from a different item, as written, can't be the one a `ref` changes.
        "read(ref spots[0], spots[1])",
        "read(ref unit.position, unit.home)",
        "unit.absorb(ref units[0])",
        "units[i].look(ref unit.home)",
        // What's worked out from a place is a value of its own.
        "more(ref spots, len(spots))",
        "other = spots[j]\nread(ref spots[i], other)",
        // A method keeping what it reads from its own value isn't given it as `mut`.
        "unit.keep(unit.position)",
    ] {
        assert_eq!(given(&format!("{call}\nstdout.print(unit.position.x, len(spots))\n")), empty, "{call}");
    }
}

#[test]
fn overlapping_arguments_are_found_wherever_the_call_is() {
    for body in [
        "if i == 0:\n    bump(ref spots[i], ref spots[j])\n",
        "for k in range(2):\n    i = k\n    bump(ref spots[i], ref spots[j])\n",
        "x = match i:\n    case 0:\n        bump(ref spots[i], ref spots[j])\n        1\n    case _:\n        2\nstdout.print(x)\n",
        "def inner(ref spots: list[Spot]):\n    bump(ref spots[0], ref spots[0])\ninner(ref spots)\n",
    ] {
        let said = given(body);
        assert_eq!(said.len(), 1, "{body}: {said:?}");
        assert!(said.iter().all(|one| one.contains(" overlap @ ")), "{body}: {said:?}");
    }
}

/// Each way of reaching one thing twice while a call changes it, `f(ref xs[i], ref xs[j])`,
/// `f(ref x, ref x)`, `bag.grab(bag.items)` and an argument that changes what its call changes,
/// says the same rule first, then its own fix, so they read as one rule.
#[test]
fn every_way_in_twice_says_the_same_rule() {
    let rule = "a call has to be the only way to reach what it changes: ";
    let program = "struct Bag:
    items: list[int]

    def grab(ref self, items: list[int]):
        self.items.append(len(items))

def both(ref a: int, ref b: int):
    a += b

def add(ref xs: list[int], n: int):
    xs.append(n)

def main(stdout: Stdout):
    mut xs = [1, 2]
    mut bag = Bag(items=[])
    i = 0
    j = 1
    both(ref xs[i], ref xs[j])
    both(ref xs[0], ref xs[0])
    bag.grab(bag.items)
    add(ref bag.items, bag.items.pop() else 0)
    stdout.print(xs, bag.items)
";
    let said = helps(program);
    assert_eq!(said.len(), 4, "{said:?}");
    assert!(said.iter().all(|help| help.starts_with(rule)), "{said:?}");
}
