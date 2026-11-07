//! `shared Name:` types: a handle to one cell, changed only through its methods, which lock it.

use crate::problems;

const ACCOUNT: &str =
    "shared Account:\n    total = 0\n\n    def add(ref self, amount: int):\n        self.total += amount\n\n";

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
         def deposit(ref self, amount: int):\n        self.main.add(amount)\n        self.accounts[0].add(1)\n\n\
         def main(stdout: Stdout):\n    bank = Bank(main=Account())\n    bank.deposit(5)\n    \
         with bank as ref held:\n        held.main.add(1)\n        with held.main as ref inner:\n            \
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
         with account as ref held:\n        with bank as ref outer:\n            pass\n    stdout.print(1)\n"
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
        "{ACCOUNT}def transfer(a: Account, b: Account, amount: int):\n    with a, b as ref x, ref y:\n        \
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
    assert_eq!(
        problems(&with("    with a, b as x, y:\n        x.total = 1\n")),
        ["`x` isn't `ref` @ x.total"]
    );
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
        problems(&with("    with a as ref x:\n        x = b\n")),
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
            "shared Log:\n    count = 0\n\n    def set(ref self, n: int):\n        self.count = checked(n)?\n"
        ),
        empty
    );
    assert_eq!(
        locking(
            "def drain(a: Account):\n    with a as ref x:\n        if x.total < 0:\n            \
             fail TooLow(total=x.total)\n        x.total = 0\n"
        ),
        empty
    );
}

#[test]
fn locks_never_nest() {
    assert_eq!(
        locking(
            "def pay(a: Account, b: Account):\n    with a as ref x:\n        b.add(1)\n        \
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
             def take(ref self, source: Account):\n        source.add(-1)\n\n    def give(self, to: Account):\n        \
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

    def add(ref self, by: int):
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
        &format!("one = Cell()\nwith one as ref locked:\n    locked.count += 1\n    {body}\n"),
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
            format!("one = Cell()\n{lines}\nwith one as ref locked:\n    locked.count += 1\n    {body}\n");
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

const SCORES: &str = "struct Scores:\n    best = dict[str, int]()\n\n    def add(ref self, name: str, points: int):\n        self.best[name] = points\n\n";

/// `shared T` holds any type, made with `shared name = value` and reached only inside a `lock`:
/// to read, or with `ref` to change. Its methods are called on the name the `lock` gives.
#[test]
fn a_shared_value_is_reached_only_in_a_lock() {
    let empty: [&str; 0] = [];
    let good = format!(
        "{SCORES}def record(scores: shared Scores, name: str):\n    lock scores as ref current:\n        current.best[name] = 1\n        current.add(name, points=2)\n\ndef main(stdout: Stdout):\n    shared scores = Scores()\n    record(scores, \"a\")\n    lock scores as current:\n        stdout.print(current.best)\n"
    );
    assert_eq!(problems(&good), empty);
    let in_main = |body: &str| {
        problems(&format!("{SCORES}def main(stdout: Stdout):\n    shared scores = Scores()\n{body}"))
    };
    assert_eq!(
        in_main("    stdout.print(scores.best)\n"),
        ["`scores` is `shared`, so it's reached inside a `lock` @ scores.best"]
    );
    assert_eq!(
        in_main("    scores.add(\"a\", points=1)\n"),
        ["`scores` is `shared`, so it's reached inside a `lock` @ scores.add(\"a\", points=1)"]
    );
    assert_eq!(
        in_main("    lock scores as current:\n        current.best[\"a\"] = 1\n"),
        ["`current` isn't `ref` @ current.best[\"a\"]"]
    );
    assert_eq!(
        in_main("    with scores as ref current:\n        pass\n"),
        ["a `shared` value is reached with `lock`, not `with` @ scores"]
    );
    assert_eq!(
        in_main("    plain = Scores()\n    lock plain as p:\n        pass\n"),
        ["`lock` takes `shared` values, but this is a Scores @ plain"]
    );
    assert_eq!(
        in_main("    lock scores as a:\n        lock scores as b:\n            pass\n"),
        ["this `lock` waits for another lock while `a` is locked @ scores"]
    );
    assert_eq!(
        in_main("    shared again = scores\n"),
        ["this is a shared Scores already, so `shared` here has nothing to do @ scores"]
    );
}

/// A function that gives back `with T` is a source: it lends a `T` to its caller's `with` block
/// with exactly one `lend`, outside any loop. It's only
/// used as what a `with` takes, and the block's name is what it lends.
#[test]
fn a_with_source_lends_once_to_a_with_block() {
    let errors = "error Oops:\n    why: str\n\ndef risky() -> int fails Oops:\n    1\n\n";
    let empty: [&str; 0] = [];
    let good = format!(
        "{errors}def opened(name: str) -> with str fails Oops:\n    _ = risky()?\n    lend name + \"!\" else:\n        pass\n\ndef sized(name: str) -> int fails Oops:\n    with opened(name)? as text:\n        return text.len()\n    0\n"
    );
    assert_eq!(problems(&good), empty);
    // A last line after a block that always returns still takes the return type.
    let after = format!(
        "{errors}def opened(name: str) -> with str fails Oops:\n    lend name\n\ndef named(name: str) -> list[str] fails Oops:\n    with opened(name)? as text:\n        return [text]\n    []\n"
    );
    assert_eq!(problems(&after), empty);
    let found = |body: &str| -> Vec<String> {
        problems(&format!("{errors}{body}"))
            .iter()
            .map(|problem| problem.split(" @ ").next().unwrap_or_default().to_owned())
            .collect()
    };
    assert_eq!(
        found("def two() -> with int:\n    lend 1\n    lend 2\n"),
        ["`two` lends twice, but a `with` source lends once"]
    );
    assert_eq!(
        found("def none() -> with int:\n    pass\n"),
        ["`none` gives back `with`, so it lends an int with `lend`"]
    );
    // The end of the block is the finish: cleanup after the `lend` may fail, and its failure is
    // the `with` statement's.
    assert_eq!(found("def late() -> with int fails Oops:\n    lend 1\n    x = risky()?\n"), empty);
    assert_eq!(
        found("def looped() -> with int:\n    for n in range(2):\n        lend n\n"),
        ["this `lend` is in a loop, so it could lend more than once"]
    );
    assert_eq!(
        found("def stray():\n    lend 1\n"),
        ["`lend` is only in a function that gives back `with T`"]
    );
    assert_eq!(found("def wrong() -> with str:\n    lend 1\n"), ["`wrong` lends a str, but this is an int"]);
    assert_eq!(
        found("def one() -> with int:\n    lend 1\n\ndef main(stdout: Stdout):\n    x = one()\n"),
        ["this lends an int for a block, so it's used in a `with`"]
    );
    // A failure in the block runs the `else:`, so a `?` that might not run isn't built there yet.
    assert_eq!(
        found(
            "def one() -> with int:\n    lend 1\n\ndef f(b: bool) -> int fails Oops:\n    with one() as n:\n        return n if b else risky()?\n"
        ),
        ["this `?` might not run, and in a source's block that isn't built yet"]
    );
}
