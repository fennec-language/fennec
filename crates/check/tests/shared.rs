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
