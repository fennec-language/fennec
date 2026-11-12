//! A local that's changed and then never read, returned or passed on is a compile error.

use crate::{helps, in_main, problems};

const NONE: [&str; 0] = [];

const UNIT: &str = "struct Unit:
    health = 10
    tags = list[str]()

    def heal(ref self):
        self.health += 1

    def healed(ref self) -> int:
        self.health += 1
        return self.health

def find(units: list[Unit], at: int) -> Unit:
    return units[at]

";

/// A block one level in, for the body of a function.
fn indented(body: &str) -> String {
    body.lines().flat_map(|line| ["    ", line, "\n"]).collect()
}

fn after_units(body: &str) -> Vec<String> {
    let indented = indented(body);
    problems(&format!("{UNIT}def run(ref units: list[Unit], stdout: Stdout) -> int:\n{indented}    0\n"))
}

#[test]
fn a_change_to_a_copy_nothing_reads_is_lost() {
    assert_eq!(
        after_units("mut unit = find(units, 0)\nunit.health = 1\n"),
        ["this changes your own copy of what `find` gave back, and nothing reads it @ unit.health = 1"]
    );
    assert_eq!(
        after_units("mut unit = find(units, 0)\nunit.tags.append(\"x\")\n"),
        [
            "this changes your own copy of what `find` gave back, and nothing reads it @ unit.tags.append(\"x\")"
        ]
    );
    assert_eq!(
        after_units("mut unit = find(units, 0)\nunit.heal()\n"),
        ["this changes your own copy of what `find` gave back, and nothing reads it @ unit.heal()"]
    );
    assert_eq!(
        in_main("mut total = 0\ntotal += 1\n"),
        ["this change to `total` is lost: nothing reads `total` after this @ total += 1"]
    );
    assert_eq!(
        in_main("mut xs = [1]\nxs[0] = 2\n"),
        ["this change to `xs` is lost: nothing reads `xs` after this @ xs[0] = 2"]
    );
    assert_eq!(
        in_main("mut xs = [1]\nstdout.print(xs)\nxs.append(2)\n"),
        ["this change to `xs` is lost: nothing reads `xs` after this @ xs.append(2)"]
    );
}

#[test]
fn a_copy_of_a_table_slot_says_so() {
    let text = format!(
        "{UNIT}def run(by_name: dict[str, Unit]):\n    mut unit = by_name.get(\"a\") else Unit()\n    unit.health = 1\n"
    );
    assert_eq!(
        problems(&text),
        ["this changes your own copy of what's in `by_name`, and nothing reads it @ unit.health = 1"]
    );
    assert_eq!(helps(&text), ["to change `by_name`, write to it there, or take out the change"]);
    // What a type makes isn't a copy of anything.
    assert_eq!(
        problems(&format!("{UNIT}def run():\n    mut unit = Unit()\n    unit.health = 1\n")),
        ["this change to `unit` is lost: nothing reads `unit` after this @ unit.health = 1"]
    );
}

#[test]
fn a_value_given_again_before_a_read_loses_the_first() {
    assert_eq!(
        in_main("mut count = 1\ncount += 1\ncount = 5\nstdout.print(count)"),
        ["this change to `count` is lost: nothing reads `count` after this @ count += 1"]
    );
}

#[test]
fn a_test_cant_lose_a_change_either() {
    assert_eq!(
        problems("test \"grows\":\n    mut xs = [1]\n    xs.append(2)\n"),
        ["this change to `xs` is lost: nothing reads `xs` after this @ xs.append(2)"]
    );
    assert_eq!(
        problems("test \"grows\":\n    mut xs = [1]\n    xs.append(2)\n    assert xs.len() == 2\n"),
        NONE
    );
}

#[test]
fn only_the_first_lost_change_to_a_name_is_reported() {
    assert_eq!(after_units("mut unit = find(units, 0)\nunit.health = 1\nunit.health = 2\n").len(), 1);
}

#[test]
fn anything_later_reading_it_keeps_the_change() {
    for body in [
        "mut unit = find(units, 0)\nunit.health = 1\nstdout.print(unit.health)\n",
        "mut unit = find(units, 0)\nunit.health = 1\nunits.append(unit)\n",
        "mut unit = find(units, 0)\nunit.health = 1\nreturn unit.health\n",
        "mut unit = find(units, 0)\nunit.health = 1\nstdout.print(f\"{unit.health}\")\n",
        // Reading another field reads the value.
        "mut unit = find(units, 0)\nunit.health = 1\nstdout.print(unit.tags)\n",
        // A call whose result is used reads what it's called on.
        "mut unit = find(units, 0)\nstdout.print(unit.healed())\n",
        "mut unit = find(units, 0)\n_ = unit.healed()\n",
        // Passing it on, even as `mut`, is up to what it's passed to.
        "mut unit = find(units, 0)\ngrow(ref unit)\n",
    ] {
        let text = format!(
            "{UNIT}def grow(ref unit: Unit):\n    unit.health += 1\n\ndef run(ref units: list[Unit], stdout: Stdout) -> int:\n{}    0\n",
            indented(body)
        );
        assert_eq!(problems(&text), NONE, "{body}");
    }
}

#[test]
fn a_branch_keeps_a_change_if_any_way_on_reads_it() {
    let reads_in_one =
        "mut total = 0\nif len(units) > 0:\n    total = 1\nif len(units) > 1:\n    stdout.print(total)\n";
    assert_eq!(after_units(reads_in_one), NONE);
    let lost_in_one = "mut total = 0\nif len(units) > 0:\n    stdout.print(total)\nelse:\n    total = 1\n";
    assert_eq!(
        after_units(lost_in_one),
        ["this change to `total` is lost: nothing reads `total` after this @ total = 1"]
    );
    let matched = "mut total = 0\nmatch len(units):\n    case 0:\n        total = 1\n    case _:\n        pass\nstdout.print(total)\n";
    assert_eq!(after_units(matched), NONE);
}

#[test]
fn a_loop_keeps_a_change_a_later_time_round_reads() {
    let later_round = "mut last = 0\nfor at in range(3):\n    stdout.print(last)\n    last = at\n";
    assert_eq!(after_units(later_round), NONE);
    let counted = "mut left = 3\nwhile left > 0:\n    left -= 1\n";
    assert_eq!(after_units(counted), NONE);
    let at_exit = "mut seen = 0\nfor at in range(3):\n    if at == 1:\n        break\n    seen += at\nstdout.print(seen)\n";
    assert_eq!(after_units(at_exit), NONE);
    let at_continue = "mut seen = 0\nfor at in range(3):\n    seen = at\n    if at == 1:\n        continue\n    stdout.print(seen)\n";
    assert_eq!(after_units(at_continue), NONE);
    let never = "mut seen = 0\nfor at in range(3):\n    seen = at\n";
    assert_eq!(
        after_units(never),
        ["this change to `seen` is lost: nothing reads `seen` after this @ seen = at"]
    );
    let fresh_each_time =
        "for at in range(3):\n    mut one = [at]\n    one.append(1)\n    stdout.print(one)\n";
    assert_eq!(after_units(fresh_each_time), NONE);
}

#[test]
fn what_a_change_reaches_outside_isnt_lost() {
    // A `mut` parameter is the caller's to read.
    assert_eq!(problems(&format!("{UNIT}def run(ref unit: Unit):\n    unit.health = 1\n")), NONE);
    // A `for mut` writes its item back.
    assert_eq!(after_units("for ref unit in units:\n    unit.health = 1\n"), NONE);
    // A nested function can read it whenever it's called.
    assert_eq!(
        after_units(
            "mut seen = [1]\ndef show() -> int:\n    seen.len()\nseen.append(2)\nstdout.print(show())\n"
        ),
        NONE
    );
}

#[test]
fn a_local_that_does_more_than_hold_isnt_lost() {
    // Through a capability, a method can do what no read shows.
    let logger = "struct Logger:\n    out: Stdout\n    lines = 0\n\n    def log(ref self):\n        self.lines += 1\n        self.out.print(self.lines)\n\n";
    assert_eq!(
        problems(&format!(
            "{logger}def main(stdout: Stdout):\n    mut logs = Logger(out=stdout)\n    logs.log()\n"
        )),
        NONE
    );
    let given = "struct Tally:\n    lines = 0\n\n    def show(ref self, out: Stdout):\n        self.lines += 1\n        out.print(self.lines)\n\n";
    assert_eq!(
        problems(&format!(
            "{given}def main(stdout: Stdout):\n    mut tally = Tally()\n    tally.show(stdout)\n"
        )),
        NONE
    );
    let handle = "struct Account:\n    total = 0\n\n    def add(ref self, amount: int):\n        self.total += amount\n\n\
                  def main(stdout: Stdout):\n    shared held = Account()\n    lock held as ref account:\n        account.add(1)\n";
    assert_eq!(problems(handle), NONE);
}

#[test]
fn a_mut_name_read_from_a_place_is_your_own_copy() {
    assert_eq!(
        after_units("mut target = units[0]\ntarget.health -= 10\n"),
        ["this changes your own copy of what's in `units`, and nothing reads it @ target.health -= 10"]
    );
    // Read, it's what the language page calls the one silent mistake left: yours, and allowed.
    assert_eq!(
        after_units("mut target = units[0]\ntarget.health -= 10\nstdout.print(target.health)\n"),
        NONE
    );
    // What it came from can be used beside it, since they're apart.
    assert_eq!(
        after_units(
            "mut target = units[0]\ntarget.health -= 10\nunits[0].health += 1\nstdout.print(target.health, units)\n"
        ),
        NONE
    );
    assert_eq!(after_units("mut first = units\nfirst.append(Unit())\nstdout.print(first, units)\n"), NONE);
}
