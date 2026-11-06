//! `shared Name:` types: a handle to one cell, changed only through its methods, which lock it.

use crate::problems;

const ACCOUNT: &str =
    "shared Account:\n    total = 0\n\n    def add(mut self, amount: int):\n        self.total += amount\n\n";

#[test]
fn any_name_for_a_handle_can_call_a_method_that_changes_it() {
    let text = format!(
        "{ACCOUNT}def pay(account: Account):\n    account.add(5)\n\ndef main(stdout: Stdout):\n    \
         account = Account()\n    same = account\n    pay(account)\n    same.add(1)\n    stdout.print(account.total)\n"
    );
    let empty: [&str; 0] = [];
    assert_eq!(problems(&text), empty);
}

#[test]
fn a_handles_fields_change_only_in_its_methods() {
    let text =
        format!("{ACCOUNT}def main(stdout: Stdout):\n    mut account = Account()\n    account.total = 5\n");
    assert_eq!(
        problems(&text),
        ["`account` is `shared`, so change it in one of its methods @ account.total"]
    );
}

#[test]
fn a_shared_value_holds_another_and_takes_its_lock_inside_its_own() {
    let text = format!(
        "{ACCOUNT}shared Bank:\n    main: Account\n    accounts = list[Account]()\n\n    \
         def deposit(mut self, amount: int):\n        self.main.add(amount)\n        self.accounts[0].add(1)\n\n\
         def main(stdout: Stdout):\n    bank = Bank(main=Account())\n    bank.deposit(5)\n    \
         with bank as mut held:\n        held.main.add(1)\n        with held.main as mut inner:\n            \
         inner.total += 1\n    stdout.print(bank.main.total)\n"
    );
    let empty: [&str; 0] = [];
    assert_eq!(problems(&text), empty);
}

#[test]
fn shared_values_never_hold_each_other_in_a_cycle() {
    let text = "shared A:\n    b: B?\n\nshared B:\n    a: A?\n\nshared Node:\n    next: list[Node]\n";
    assert_eq!(
        problems(text),
        [
            "`A` holds `B`, which holds `A` again, and `shared` types can't hold each other in a cycle @ b: B?",
            "`B` holds `A`, which holds `B` again, and `shared` types can't hold each other in a cycle @ a: A?",
            "`Node` holds itself, and `shared` types can't hold each other in a cycle @ next: list[Node]",
        ]
    );
}

#[test]
fn a_lock_body_takes_only_inner_locks() {
    let text = format!(
        "{ACCOUNT}shared Bank:\n    main: Account\n\n\
         def main(stdout: Stdout):\n    bank = Bank(main=Account())\n    account = bank.main\n    \
         with account as mut held:\n        with bank as mut outer:\n            pass\n    stdout.print(1)\n"
    );
    assert_eq!(problems(&text), ["this `with` takes another lock while `held` is locked @ bank"]);
}

#[test]
fn a_shared_value_cant_be_a_constant() {
    let text =
        format!("{ACCOUNT}BANK = Account()\n\ndef main(stdout: Stdout):\n    stdout.print(BANK.total)\n");
    assert_eq!(
        problems(&text),
        ["`BANK` holds `Account`, which is `shared`, so it can't be a constant @ BANK = Account()"]
    );
}

#[test]
fn a_with_block_changes_handles_through_the_names_it_locks() {
    let text = format!(
        "{ACCOUNT}def transfer(a: Account, b: Account, amount: int):\n    with a, b as mut x, mut y:\n        \
         x.total -= amount\n        y.add(amount)\n\ndef main(stdout: Stdout):\n    a = Account(total=10)\n    \
         b = Account()\n    transfer(a, b, 3)\n    mut total = 0\n    with a as seen:\n        \
         total = seen.total\n    stdout.print(total)\n"
    );
    let empty: [&str; 0] = [];
    assert_eq!(problems(&text), empty);
}

#[test]
fn a_with_block_locks_only_handles_and_names_each_one() {
    let with = |body: &str| {
        format!(
            "{ACCOUNT}def main(stdout: Stdout):\n    a = Account()\n    b = Account()\n{body}        pass\n"
        )
    };
    assert_eq!(problems(&with("    with a, b as x, y:\n        x.total = 1\n")), ["`x` isn't mut @ x.total"]);
    assert_eq!(
        problems(&with("    with a as x:\n        x.add(1)\n")),
        ["`x` is locked to read, and `add` changes it @ x"]
    );
    assert_eq!(
        problems(&with("    with a:\n")),
        ["this `with` locks an Account, so it needs a name for it @ a"]
    );
    assert_eq!(
        problems(&with("    with a, 5 as x, y:\n")),
        ["a `with` holds several values only to lock `shared` handles, but this is an int @ 5"]
    );
    assert_eq!(
        problems(&with("    with a as mut x:\n        x = b\n")),
        ["`x` holds a lock for this `with` block, so it can't name anything else @ x"]
    );
    assert_eq!(problems(&with("    with a, b as x:\n")), ["parse: this `with` holds 2 values but names 1"]);
}

/// A program with `Account`, an error, and a fallible function, then `rest`.
fn locking(rest: &str) -> Vec<String> {
    let text = format!(
        "{ACCOUNT}error TooLow:\n    total: int\n\ndef checked(n: int) -> int:\n    if n < 0:\n        \
         fail TooLow(total=n)\n    n\n\n{rest}"
    );
    problems(&text)
}

#[test]
fn a_lock_body_can_do_io_and_leave_early() {
    let empty: [&str; 0] = [];
    assert_eq!(
        locking(
            "def main(stdout: Stdout):\n    a = Account()\n    with a as x:\n        stdout.print(x.total)\n"
        ),
        empty
    );
    assert_eq!(
        locking(
            "shared Log:\n    count = 0\n\n    def show(self, out: Stdout):\n        out.print(self.count)\n"
        ),
        empty
    );
    assert_eq!(
        locking(
            "shared Log:\n    count = 0\n\n    def set(mut self, n: int):\n        self.count = checked(n)?\n"
        ),
        empty
    );
    assert_eq!(
        locking(
            "def drain(a: Account):\n    with a as mut x:\n        if x.total < 0:\n            \
             fail TooLow(total=x.total)\n        x.total = 0\n"
        ),
        empty
    );
}

#[test]
fn locks_never_nest() {
    assert_eq!(
        locking(
            "def pay(a: Account, b: Account):\n    with a as mut x:\n        b.add(1)\n        \
             x.total = b.total\n        with b as y:\n            pass\n"
        ),
        [
            "calling `add` locks `b` while `x` is locked @ b",
            "reading `total` locks `b` while `x` is locked @ b",
            "this `with` takes another lock while `x` is locked @ b",
        ]
    );
    assert_eq!(
        locking(
            "def pay(account: Account):\n    account.add(1)\n\nshared Bank:\n    total = 0\n\n    \
             def take(mut self, source: Account):\n        source.add(-1)\n\n    def give(self, to: Account):\n        \
             pay(to)\n"
        ),
        [
            "calling `add` locks `source` while `self` is locked @ source",
            "a `shared` handle can't be passed to a call while `self` is locked @ to",
        ]
    );
}

const HOLDERS: &str = "shared Cell:
    count = 0

    def add(mut self, by: int):
        self.count += by

struct Holder:
    cell: Cell

    def poke(self):
        self.cell.add(1)

def through(holder: Holder):
    holder.poke()

def through_all(cells: list[Cell]):
    cells[0].add(1)

def apply(work: (int) -> int) -> int:
    work(1)

def run(work: () -> int) -> int:
    work()
";

fn in_a_lock(body: &str) -> Vec<String> {
    let body = body.replace('\n', "\n    ");
    let found = crate::in_main_after(
        HOLDERS,
        &format!("one = Cell()\nwith one as mut locked:\n    locked.count += 1\n    {body}\n"),
    );
    found.iter().filter_map(|said| said.split(" @ ").next().map(str::to_owned)).collect()
}

#[test]
fn what_holds_a_handle_cant_be_handed_on_under_a_lock() {
    let found = in_a_lock("through(Holder(cell=one))");
    assert_eq!(
        found,
        ["a Holder holds a `shared` `Cell`, so it can't be passed to a call while `locked` is locked"]
    );
    let found = in_a_lock("through_all([one])");
    assert_eq!(
        found,
        ["a list[Cell] holds a `shared` `Cell`, so it can't be passed to a call while `locked` is locked"]
    );
    let found = in_a_lock("holder = Holder(cell=one)\nholder.poke()");
    assert_eq!(found, ["calling `poke` on a Holder that holds a `shared` `Cell` while `locked` is locked"]);
}

#[test]
fn a_function_that_captures_a_handle_cant_run_under_a_lock() {
    let before = |lines: &str, body: &str| {
        let source =
            format!("one = Cell()\n{lines}\nwith one as mut locked:\n    locked.count += 1\n    {body}\n");
        let found = crate::in_main_after(HOLDERS, &source);
        found.iter().filter_map(|said| said.split(" @ ").next().map(str::to_owned)).collect::<Vec<_>>()
    };
    let calling = "calling `poke`, which captures a `shared` handle, could lock it while `locked` is locked";
    let nested = "def poke():\n    one.add(1)";
    assert_eq!(before(nested, "poke()"), [calling]);
    let through = "def poke():\n    one.add(1)\ndef again():\n    poke()";
    assert_eq!(
        before(through, "again()"),
        ["calling `again`, which captures a `shared` handle, could lock it while `locked` is locked"]
    );
    let stored = "work = () => one.count";
    assert_eq!(
        before(stored, "locked.count += work()"),
        ["calling `work`, which captures a `shared` handle, could lock it while `locked` is locked"]
    );
    assert_eq!(
        before(stored, "locked.count += run(work)"),
        ["`work` holds a `shared` handle, so it can't be passed to a call while `locked` is locked"]
    );
}

#[test]
fn what_holds_no_handle_goes_on_as_before_under_a_lock() {
    let empty: [&str; 0] = [];
    for body in [
        "locked.count += apply(n => n + 1)",
        "numbers = [1]\nlocked.count += numbers.len()",
        "def plain(n: int) -> int:\n    n + 1\nlocked.count += plain(1)",
        // Making a value that holds it hands it to no call.
        "holder = Holder(cell=one)\nlocked.count += 1",
    ] {
        assert_eq!(in_a_lock(body), empty, "{body}");
    }
    let outside = "one = Cell()\nholder = Holder(cell=one)\nthrough(holder)\nholder.poke()\n";
    assert_eq!(crate::in_main_after(HOLDERS, outside), empty);
}
