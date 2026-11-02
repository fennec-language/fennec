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
fn a_shared_value_cant_hold_another() {
    let text = format!("{ACCOUNT}shared Bank:\n    main: Account\n    accounts = list[Account]()\n");
    assert_eq!(
        problems(&text),
        [
            "`Bank` is `shared`, so it can't hold `Account`, which is `shared` too @ main: Account",
            "`Bank` is `shared`, so it can't hold `Account`, which is `shared` too @ accounts = list[Account]()",
        ]
    );
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
fn a_lock_body_does_no_io_and_cant_leave_early() {
    assert_eq!(
        locking(
            "def main(stdout: Stdout):\n    a = Account()\n    with a as x:\n        stdout.print(x.total)\n"
        ),
        ["`stdout` does I/O, which can't happen while `x` is locked @ stdout"]
    );
    assert_eq!(
        locking(
            "shared Log:\n    count = 0\n\n    def show(self, out: Stdout):\n        out.print(self.count)\n"
        ),
        ["`out` does I/O, which can't happen while `self` is locked @ out"]
    );
    assert_eq!(
        locking(
            "shared Log:\n    count = 0\n\n    def set(mut self, n: int):\n        self.count = checked(n)?\n"
        ),
        ["`?` can't leave while `self` is locked @ checked(n)?"]
    );
    assert_eq!(
        locking(
            "def drain(a: Account):\n    with a as mut x:\n        if x.total < 0:\n            \
             fail TooLow(total=x.total)\n        x.total = 0\n"
        ),
        ["`fail` can't leave while `x` is locked @ TooLow(total=x.total)"]
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
