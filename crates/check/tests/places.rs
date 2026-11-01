//! `mut x = place`: a name that's edited stands for the place it came from.

use crate::{helps, problems};

fn in_main(body: &str) -> String {
    let setup = "struct Stats:\n    hp = 0\n    mp = 0\n\nstruct Unit:\n    stats: Stats\n\n";
    let lines: Vec<String> = body.lines().map(|line| format!("    {line}\n")).collect();
    format!("{setup}def main(stdout: Stdout):\n    mut unit = Unit(stats=Stats())\n{}", lines.concat())
}

#[test]
fn a_name_for_a_place_can_be_edited() {
    let empty: [&str; 0] = [];
    assert_eq!(
        problems(&in_main("mut stats = unit.stats\nstats.hp -= 1\nstdout.print(unit.stats.hp)")),
        empty
    );
}

#[test]
fn a_name_thats_only_replaced_is_a_value_of_its_own() {
    let empty: [&str; 0] = [];
    let text = in_main("mut stats = unit.stats\nstats = Stats(hp=1)\nstdout.print(unit.stats.hp, stats.hp)");
    assert_eq!(problems(&text), empty);
}

#[test]
fn a_name_cant_be_both_edited_and_replaced() {
    let text = in_main("mut stats = unit.stats\nstats.hp = 1\nstats = Stats()");
    assert_eq!(problems(&text), ["`stats` edits a place and is given a new value too @ stats"]);
    assert!(helps(&text)[0].contains("copy(...)"));
}

#[test]
fn reading_a_name_after_another_changes_it_needs_a_copy() {
    let text = "def main(stdout: Stdout):\n    hp = [1]\n    mut roster = hp\n    roster.append(2)\n    stdout.print(hp)\n";
    assert_eq!(problems(text), ["`hp` is read after `roster` changes it @ hp"]);
    assert!(helps(text)[0].contains("mut roster = copy(hp)"));
}

#[test]
fn a_name_for_a_place_cant_be_returned() {
    let text = "struct Box:\n    items: list[int]\n\ndef take(mut box: Box) -> list[int]:\n    mut items = box.items\n    items.append(1)\n    return items\n";
    assert_eq!(problems(text), ["`items` names a place, so it can't be kept or given back @ items"]);
}

#[test]
fn a_name_for_a_place_cant_be_the_functions_value() {
    let text = "struct Box:\n    items: list[int]\n\ndef take(mut box: Box) -> list[int]:\n    mut items = box.items\n    items.append(1)\n    items\n";
    assert_eq!(problems(text), ["`items` names a place, so it can't be kept or given back @ items"]);
}

#[test]
fn a_name_for_a_place_cant_be_stored() {
    let text = in_main("mut stats = unit.stats\nstats.hp = 1\nunit.stats = stats");
    assert!(problems(&text).iter().any(|problem| problem.contains("can't be kept")), "{:?}", problems(&text));
}

#[test]
fn a_name_for_a_place_cant_be_kept_in_a_list() {
    let text =
        in_main("mut stats = unit.stats\nstats.hp = 1\nmut kept: list[Stats] = []\nkept.append(stats)");
    assert_eq!(problems(&text), ["`stats` names a place, so it can't be kept or given back @ stats"]);
}

#[test]
fn a_name_for_a_place_cant_be_captured() {
    let text = in_main("mut stats = unit.stats\nstats.hp = 1\nstdout.print(max([1], key=n => n + stats.hp))");
    assert_eq!(problems(&text), ["`stats` names a place, so it can't be kept or given back @ stats.hp"]);
}

#[test]
fn what_a_name_came_from_is_locked_until_its_last_use() {
    let text = in_main("mut stats = unit.stats\nstats.hp = 1\nstdout.print(unit.stats.mp)\nstats.mp = 2");
    assert_eq!(
        problems(&text),
        ["`unit` can't be used here, while `stats` names part of it @ unit.stats.mp"]
    );
}

#[test]
fn what_a_name_came_from_is_free_after_its_last_use() {
    let empty: [&str; 0] = [];
    let text = in_main("mut stats = unit.stats\nstats.hp = 1\nstats.mp = 2\nstdout.print(unit.stats.mp)");
    assert_eq!(problems(&text), empty);
}

#[test]
fn names_for_separate_fields_are_fine() {
    let empty: [&str; 0] = [];
    let text = "struct Pair:\n    one = [0]\n    two = [0]\n\ndef main(stdout: Stdout):\n    mut pair = Pair()\n    mut one = pair.one\n    mut two = pair.two\n    one.append(1)\n    two.append(2)\n    one.append(3)\n    stdout.print(pair.one, pair.two)\n";
    assert_eq!(problems(text), empty);
}

#[test]
fn a_place_in_something_that_isnt_mut_cant_be_edited() {
    let text = "struct Box:\n    items: list[int]\n\ndef main(stdout: Stdout):\n    box = Box(items=[])\n    mut items = box.items\n    items.append(1)\n    stdout.print(items)\n";
    assert_eq!(problems(text), ["`items` edits `box`, which isn't mut @ items"]);
}

#[test]
fn an_index_that_changes_while_it_picks_the_place_is_an_error() {
    let text = "def main(stdout: Stdout):\n    mut rows = [[1], [2]]\n    mut i = 0\n    mut row = rows[i]\n    i += 1\n    row.append(1)\n    stdout.print(i)\n";
    assert_eq!(problems(text), ["`i` changes while `row` names the place it picks @ i"]);
}

#[test]
fn an_index_has_to_be_plain() {
    let text = "def main(stdout: Stdout):\n    mut rows = [[1], [2]]\n    mut row = rows[rows.len() - 1]\n    row.append(1)\n";
    assert_eq!(
        problems(text),
        ["`row` names a place by an index that has to be worked out @ rows.len() - 1"]
    );
}

#[test]
fn a_name_for_a_name_keeps_the_place_locked() {
    let text = in_main(
        "mut part = unit\nmut stats = part.stats\nstats.hp = 1\nstdout.print(unit.stats.mp)\nstats.mp = 2",
    );
    assert_eq!(problems(&text), ["`unit` can't be used here, while `part` names part of it @ unit.stats.mp"]);
}
