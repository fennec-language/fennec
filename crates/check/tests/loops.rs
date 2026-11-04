//! A collection can't change while a loop goes over it, in the loop's body or in anything the
//! body calls.

use crate::{helps, in_main_after};

const TEAM: &str = "struct Team:
    names = list[str]()
    wins = 0

    def add(mut self, name: str):
        self.names.append(name)

    def win(mut self):
        self.wins += 1

def grow(mut names: list[str]):
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
    assert_eq!(changes("for name in copy(names):\n    names.append(name)\n"), empty);
    assert_eq!(changes("for name in names[1:]:\n    names.append(name)\n"), empty);
    assert_eq!(
        changes("mut found = list[str]()\nfor name in names:\n    found.append(name)\nnames = found\n"),
        empty
    );
    assert_eq!(changes("for mut name in names:\n    name = name.upper()\n"), empty);
}

#[test]
fn a_loop_cant_change_what_it_goes_over() {
    let changed = |shown: &str, at: &str| vec![format!("`{shown}` changes while a loop goes over it @ {at}")];
    assert_eq!(
        changes("for name in names:\n    names.append(name)\n"),
        changed("names", "names.append(name)")
    );
    assert_eq!(
        changes("for at, name in enumerate(names):\n    names[at] = name.upper()\n"),
        changed("names", "names[at]")
    );
    assert_eq!(changes("for name in names:\n    grow(mut names)\n"), changed("names", "names"));
    assert_eq!(
        changes("for name in team.names:\n    team.add(name)\n"),
        changed("team.names", "team.add(name)")
    );
    assert_eq!(
        changes("for key in scores.keys():\n    scores.pop(key)\n"),
        changed("scores", "scores.pop(key)")
    );
    assert_eq!(
        changes(
            "for name in names:\n    if name == \"ash\":\n        for other in names:\n            names.remove(other)\n"
        ),
        [
            "`names` changes while a loop goes over it @ names.remove(other)",
            "`names` changes while a loop goes over it @ names.remove(other)",
        ]
    );
    assert_eq!(
        helps(&format!(
            "{TEAM}\ndef main(stdout: Stdout):\n    mut names = [\"ash\"]\n    for name in names:\n        names.append(name)\n"
        )),
        ["loop over `copy(names)`, or collect the changes and make them after the loop"]
    );
}

#[test]
fn writing_through_a_collection_changes_it_for_a_loop_over_it() {
    let problem = ["`names` changes while a loop goes over it @ names"];
    for body in [
        "for mut name in names:\n    for mut other in names:\n        other += name\n",
        "for name in names:\n    for mut other in names:\n        other += name\n",
        "for mut name in names:\n    if name == \"a\":\n        for mut other in names:\n            other += name\n",
    ] {
        assert_eq!(changes(body), problem, "{body}");
    }
}

#[test]
fn a_function_that_writes_through_a_collection_changes_it() {
    let writes = "def shout(mut items: list[str]):\n    for mut item in items:\n        item = item.upper()\n\n\
                  def twice(mut items: list[str]):\n    shout(mut items)\n\n";
    let problem = ["`names` changes while a loop goes over it @ names"];
    for call in ["shout", "twice"] {
        let body = format!("mut names = [\"a\"]\nfor name in names:\n    {call}(mut names)\n");
        assert_eq!(in_main_after(writes, &body), problem, "{call}");
    }
}

#[test]
fn a_nested_for_mut_over_something_else_is_fine() {
    let empty: [&str; 0] = [];
    let body =
        "mut grid = [[1, 2], [3]]\nfor mut row in grid:\n    for mut cell in row:\n        cell += 1\n";
    assert_eq!(in_main_after("", body), empty);
    let body = "mut xs = [1]\nmut ys = [2]\nfor mut x in xs:\n    for mut y in ys:\n        y += x\n";
    assert_eq!(in_main_after("", body), empty);
    let body = "mut xs = [1]\nfor mut x in xs:\n    x += 1\nfor mut x in xs:\n    x += 1\n";
    assert_eq!(in_main_after("", body), empty);
}

#[test]
fn a_for_mut_over_an_id_map_locks_it_too() {
    let body = "mut units = IdMap[int]()\nfor mut unit in units:\n    for mut other in units:\n        other += unit\n";
    assert_eq!(
        in_main_after("from std.game import IdMap\n\n", body),
        ["`units` changes while a loop goes over it @ units"]
    );
}

#[test]
fn a_for_mut_isnt_told_to_loop_over_a_copy() {
    let help = helps(
        "def main(stdout: Stdout):\n    mut xs = [1]\n    for mut x in xs:\n        for mut y in xs:\n            y += x\n",
    );
    assert_eq!(help, ["collect the changes and make them after the loop"]);
}

const PLACES: &str = "struct Spot:
    x = 0

struct Unit:
    position = Spot()
    home = Spot()
    spots = list[Spot]()

    def absorb(mut self, mut other: Unit):
        self.position.x += other.position.x

    def look(self, mut at: Spot):
        at.x = self.position.x

    def keep(mut self, spot: Spot):
        self.spots.append(spot)

def bump(mut first: Spot, mut second: Spot):
    first.x += 1
    second.x += 1

def read(mut first: Spot, second: Spot):
    first.x = second.x

def fill(mut spots: list[Spot], spot: Spot):
    spots.append(spot)

def more(mut spots: list[Spot], count: int):
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
        ("bump(mut unit.position, mut unit.position)", "`mut unit.position` and `mut unit.position` overlap"),
        ("bump(mut spots[i], mut spots[j])", "`mut spots[i]` and `mut spots[j]` overlap"),
        ("bump(mut spots[0], mut spots[1])", "`mut spots[0]` and `mut spots[1]` overlap"),
        ("read(mut unit.position, unit.position)", "`mut unit.position` and `unit.position` overlap"),
        ("fill(mut spots, spots[i])", "`mut spots` and `spots[i]` overlap"),
        ("fill(mut unit.spots, unit.spots[i + 1])", "`mut unit.spots` and `unit.spots[...]` overlap"),
        ("unit.absorb(mut unit)", "`mut unit` overlaps `unit`, which this method is called on"),
        (
            "unit.look(mut unit.position)",
            "`mut unit.position` overlaps `unit`, which this method is called on",
        ),
    ];
    for (call, said) in cases {
        let found = given(&format!("{call}\nstdout.print(unit.position.x, spots.len())\n"));
        let messages: Vec<&str> = found.iter().filter_map(|one| one.split(" @ ").next()).collect();
        assert_eq!(messages, [said], "{call}");
    }
}

#[test]
fn different_places_can_be_given_as_mut_together() {
    let empty: [&str; 0] = [];
    for call in [
        "bump(mut unit.position, mut unit.home)",
        "bump(mut units[i].position, mut units[j].home)",
        "read(mut unit.position, unit.home)",
        "unit.absorb(mut units[0])",
        "units[i].look(mut unit.home)",
        // What's worked out from a place is a value of its own.
        "more(mut spots, spots.len())",
        "read(mut spots[i], copy(spots[j]))",
        // A method keeping what it reads from its own value isn't given it as `mut`.
        "unit.keep(unit.position)",
    ] {
        assert_eq!(given(&format!("{call}\nstdout.print(unit.position.x, spots.len())\n")), empty, "{call}");
    }
}

#[test]
fn overlapping_arguments_are_found_wherever_the_call_is() {
    for body in [
        "if i == 0:\n    bump(mut spots[i], mut spots[j])\n",
        "for k in range(2):\n    i = k\n    bump(mut spots[i], mut spots[j])\n",
        "x = match i:\n    case 0:\n        bump(mut spots[i], mut spots[j])\n        1\n    case _:\n        2\nstdout.print(x)\n",
        "def inner(mut spots: list[Spot]):\n    bump(mut spots[0], mut spots[0])\ninner(mut spots)\n",
    ] {
        let said = given(body);
        assert_eq!(said.len(), 1, "{body}: {said:?}");
        assert!(said.iter().all(|one| one.contains(" overlap @ ")), "{body}: {said:?}");
    }
}
