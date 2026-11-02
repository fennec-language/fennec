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
        &format!("mut names = [\"ash\", \"kit\"]\nmut team = Team()\nmut scores = {{\"a\": 1}}\n{body}"),
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
