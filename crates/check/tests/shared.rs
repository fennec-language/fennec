//! `shared T`: a value many tasks reach, each inside a `lock`, which replaced `shared Name:`
//! declarations and `with` on them.

use crate::{helps, problems};

const ACCOUNT: &str =
    "struct Account:\n    total = 0\n\n    def add(ref self, amount: int):\n        self.total += amount\n\n";

/// A `shared Name:` declaration and a `with` on its values are refused, and their fix writes in
/// what's mechanical: `struct`, `shared` on each type and new name, and `lock`.
#[test]
fn a_shared_declaration_and_a_with_on_it_are_refused_with_a_fix() {
    let program = "shared Account:\n    total = 0\n\ndef pay(account: Account):\n    with account as ref held:\n        held.total += 5\n\n\
        def main(stdout: Stdout):\n    account = Account()\n    pay(account)\n    with account as seen:\n        stdout.print(seen.total)\n";
    assert_eq!(
        problems(program),
        [
            "`shared Account:` is gone: a type is declared with `struct`, and `shared` goes where a value is shared @ shared",
            "a `shared` value is reached with `lock`, not `with` @ account",
            "a `shared` value is reached with `lock`, not `with` @ account",
        ]
    );
    let module = fennec_syntax::parse(program).unwrap();
    let diagnostics = fennec_check::check(&module).unwrap_err();
    let fixed = fennec_syntax::apply_fixes(program, 0, &diagnostics).unwrap();
    assert_eq!(
        fixed,
        "struct Account:\n    total = 0\n\ndef pay(account: shared Account):\n    lock account as ref held:\n        held.total += 5\n\n\
        def main(stdout: Stdout):\n    shared account = Account()\n    pay(account)\n    lock account as seen:\n        stdout.print(seen.total)\n"
    );
    let empty: [&str; 0] = [];
    assert_eq!(problems(&fixed), empty);
}

#[test]
fn any_name_for_a_shared_value_locks_the_same_one() {
    let text = format!(
        "{ACCOUNT}def pay(account: shared Account):\n    lock account as ref held:\n        held.add(5)\n\n\
         def main(stdout: Stdout):\n    shared account = Account()\n    same = account\n    pay(account)\n    \
         lock same as ref held:\n        held.add(1)\n    lock account as seen:\n        stdout.print(seen.total)\n"
    );
    let empty: [&str; 0] = [];
    assert_eq!(problems(&text), empty);
}

/// Showing or comparing a value that holds a `shared` one reads it under that value's lock, so
/// inside a lock body nothing shows or compares one, as nothing passes one to a call there:
/// printing, an f-string, `str`, an `assert` that would say what each side was, and `==` are
/// refused, the locked value's own handle and a value holding another `shared` one alike.
#[test]
fn a_shared_value_isnt_shown_inside_a_lock_body() {
    let text = format!(
        "{ACCOUNT}struct Bank:\n    accounts: list[shared Account]\n\n\
         def main(stdout: Stdout):\n    shared account = Account()\n    bank = Bank(accounts=[account])\n    \
         lock account as ref held:\n        held.add(1)\n        stdout.print(account)\n        stdout.print(f\"{{bank}}\")\n        \
         text = str(bank)\n        assert [account] != []\n        same = account == account\n        stdout.print(held.total, text, same)\n"
    );
    let shown = "would take the lock of the `shared` value in it while `held` is locked";
    assert_eq!(
        problems(&text),
        [
            format!("printing this {shown} @ account"),
            format!("this f-string {shown} @ bank"),
            format!("`str` of this {shown} @ bank"),
            format!("comparing this {shown} @ [account]"),
            format!("comparing this {shown} @ account"),
        ]
    );
}

#[test]
fn a_shared_values_fields_change_only_in_a_lock() {
    let text = format!(
        "{ACCOUNT}def main(stdout: Stdout):\n    shared account = Account()\n    account.total = 5\n"
    );
    assert_eq!(problems(&text), ["`account` is `shared`, so it's reached inside a `lock` @ account.total"]);
}

/// A `shared` value can hold another, but taking the inner one's lock inside the outer's is
/// refused for now, however many calls down: `shared Name:` allowed it, inner lock first.
#[test]
fn a_lock_on_what_a_shared_value_holds_is_refused_inside_its_own() {
    let text = format!(
        "{ACCOUNT}struct Bank:\n    main: shared Account\n    accounts: list[shared Account] = []\n\n    \
         def deposit(ref self, amount: int):\n        lock self.main as ref main:\n            main.add(amount)\n\n\
         def main(stdout: Stdout):\n    shared first = Account()\n    shared bank = Bank(main=first)\n    \
         lock bank as ref held:\n        held.deposit(5)\n    lock first as ref one:\n        one.add(1)\n"
    );
    assert_eq!(
        problems(&text),
        ["`deposit` takes `self.main`'s lock while `held` is locked @ held.deposit(5)"]
    );
}

#[test]
fn shared_values_never_hold_each_other_in_a_cycle() {
    let text = "struct A:\n    b: (shared B)?\n\nstruct B:\n    a: (shared A)?\n\nstruct Node:\n    next: list[shared Node]\n";
    assert_eq!(
        problems(text),
        [
            "a `shared A` holds a `shared B`, which holds a `shared A` again, and `shared` values can't hold each other in a cycle @ b: (shared B)?",
            "a `shared B` holds a `shared A`, which holds a `shared B` again, and `shared` values can't hold each other in a cycle @ a: (shared A)?",
            "a `shared Node` can hold itself, and `shared` values can't hold each other in a cycle @ next: list[shared Node]",
        ]
    );
}

#[test]
fn a_lock_body_takes_no_other_lock() {
    let text = format!(
        "{ACCOUNT}struct Bank:\n    main: shared Account\n\n\
         def main(stdout: Stdout):\n    shared first = Account()\n    shared bank = Bank(main=first)\n    \
         lock first as ref held:\n        lock bank as ref outer:\n            pass\n    stdout.print(1)\n"
    );
    assert_eq!(problems(&text), ["this `lock` waits for another lock while `held` is locked @ bank"]);
}

#[test]
fn a_shared_value_cant_be_a_constant() {
    let text = format!(
        "{ACCOUNT}BANK: shared Account = Account()\n\ndef main(stdout: Stdout):\n    stdout.print(1)\n"
    );
    assert_eq!(problems(&text), ["`BANK` should be a shared Account, but this is an Account @ Account()"]);
}

#[test]
fn a_lock_changes_values_through_the_names_it_gives() {
    let text = format!(
        "{ACCOUNT}def transfer(a: shared Account, b: shared Account, amount: int):\n    lock a, b as ref x, ref y:\n        \
         x.total -= amount\n        y.add(amount)\n\ndef main(stdout: Stdout):\n    shared a = Account(total=10)\n    \
         shared b = Account()\n    transfer(a, b, amount=3)\n    mut total = 0\n    lock a as seen:\n        \
         total = seen.total\n    stdout.print(total)\n"
    );
    let empty: [&str; 0] = [];
    assert_eq!(problems(&text), empty);
}

#[test]
fn a_lock_takes_only_shared_values_and_names_each_one() {
    let lock = |body: &str| {
        format!(
            "{ACCOUNT}def main(stdout: Stdout):\n    shared a = Account()\n    shared b = Account()\n{body}        pass\n"
        )
    };
    assert_eq!(
        problems(&lock("    lock a, b as x, y:\n        x.total = 1\n")),
        ["`x` isn't `ref` @ x.total"]
    );
    assert_eq!(problems(&lock("    lock a as x:\n        x.add(1)\n")), ["`x` isn't `ref` @ x"]);
    assert_eq!(problems(&lock("    lock a:\n")), ["parse: a `lock` names what it holds, for the block"]);
    assert_eq!(
        problems(&lock("    lock a, 5 as x, y:\n")),
        ["`lock` takes `shared` values, but this is an int @ 5"]
    );
    assert_eq!(
        problems(&lock("    lock a as ref x:\n        x = b\n")),
        ["`x` names what this `lock` holds, so it can't name anything else @ x"]
    );
    assert_eq!(problems(&lock("    lock a, b as x:\n")), ["parse: this `lock` holds 2 values but names 1"]);
}

/// A program with `Account`, an error, and a fallible function, then `rest`.
fn locking(rest: &str) -> Vec<String> {
    let text = format!(
        "{ACCOUNT}error TooLow:\n    total: int\n\ndef checked(n: int) -> int:\n    if n < 0:\n        \
         fail TooLow(total=n)\n    n\n\n{rest}"
    );
    problems(&text)
}

/// One `lock` naming one `shared` value twice, by the same name or the same path, is refused where
/// the second is written, with how to take it once (#2230). Two values the checker can't tell
/// apart, as two items of one list, are taken together, and stop the program if they're the same.
#[test]
fn a_lock_naming_one_value_twice_is_refused() {
    let twice = "def main(stdout: Stdout):\n    shared a = Account()\n    \
                 lock a, a as ref x, ref y:\n        x.add(1)\n        y.add(1)\n";
    assert_eq!(locking(twice), ["this `lock` takes `a` twice, but it's one `shared` value @ a"]);
    let text = format!("{ACCOUNT}{twice}");
    assert_eq!(helps(&text), ["take it once, as `lock a as ref x:`, and use `x` for both"]);
    let path = "struct Pair:\n    left: shared Account\n    right: shared Account\n\n\
                def touch(pair: Pair):\n    lock pair.left, pair.left as ref x, ref y:\n        x.add(1)\n";
    assert_eq!(
        locking(path),
        ["this `lock` takes `pair.left` twice, but it's one `shared` value @ pair.left"]
    );
    let empty: [&str; 0] = [];
    let apart = "struct Pair:\n    left: shared Account\n    right: shared Account\n\n\
                 def touch(pair: Pair):\n    lock pair.left, pair.right as ref x, ref y:\n        x.add(1)\n        y.add(2)\n";
    assert_eq!(locking(apart), empty);
    let items = "def touch(accounts: list[shared Account], from_id: int, to_id: int):\n    \
                 lock accounts[from_id], accounts[to_id] as ref source, ref target:\n        \
                 source.add(-1)\n        target.add(1)\n";
    assert_eq!(locking(items), empty);
}

#[test]
fn a_lock_body_can_do_io_and_leave_early() {
    let empty: [&str; 0] = [];
    assert_eq!(
        locking(
            "def main(stdout: Stdout):\n    shared a = Account()\n    lock a as x:\n        stdout.print(x.total)\n"
        ),
        empty
    );
    assert_eq!(
        locking(
            "def drain(a: shared Account):\n    lock a as ref x:\n        if x.total < 0:\n            \
             fail TooLow(total=x.total)\n        x.total = checked(x.total)?\n"
        ),
        empty
    );
}

#[test]
fn locks_never_nest() {
    assert_eq!(
        locking(
            "def pay(a: shared Account, b: shared Account):\n    lock a as ref x:\n        lock b as ref y:\n            \
             y.add(1)\n            x.total = y.total\n"
        ),
        ["this `lock` waits for another lock while `x` is locked @ b"]
    );
    assert_eq!(
        locking(
            "def give(account: shared Account):\n    lock account as ref held:\n        held.add(1)\n\n\
             struct Bank:\n    total = 0\n\n    def take(ref self, source: shared Account):\n        \
             lock source as ref s:\n            s.add(-1)\n\n\
             def main(stdout: Stdout):\n    shared a = Account()\n    shared bank = Bank()\n    \
             lock bank as ref held:\n        held.take(a)\n        give(a)\n"
        ),
        [
            "`take` takes `source`'s lock while `held` is locked @ held.take(a)",
            "`give` takes `account`'s lock while `held` is locked @ give(a)",
        ]
    );
}

/// A nested function called inside a `lock` takes a lock only if it does, or something it calls
/// does: one that takes none is fine, and one that takes another lock, itself or through a function
/// it calls, is refused. The function it's written in takes what it takes where it calls it.
/// Before, a nested function counted as the function around it, which took the lock, so every one
/// called inside it was refused.
#[test]
fn a_nested_function_inside_a_lock_takes_only_what_it_takes() {
    let empty: [&str; 0] = [];
    let takes_none = "def main(stdout: Stdout):\n    shared log = list[int]()\n    lock log as ref lines:\n        \
        lines.append(5)\n        def counted(other: list[int]) -> int:\n            return len(other)\n        \
        stdout.print(counted([1, 2, 3]))\n        stdout.print(len(lines))\n";
    assert_eq!(locking(takes_none), empty);
    let takes_another = "def give(account: shared Account):\n    lock account as ref held:\n        held.add(1)\n\n\
        def main(stdout: Stdout):\n    shared log = list[int]()\n    shared other = Account()\n    \
        def poke():\n        lock other as ref held:\n            held.add(1)\n    \
        def relayed():\n        give(other)\n    \
        lock log as ref lines:\n        lines.append(5)\n        poke()\n        relayed()\n";
    assert_eq!(
        locking(takes_another),
        [
            "`poke` takes `other`'s lock while `lines` is locked @ poke()",
            "`relayed` takes `account`'s lock while `lines` is locked @ relayed()",
        ]
    );
    let around_it = "def outer(account: shared Account):\n    def poke():\n        lock account as ref held:\n            \
        held.add(1)\n    poke()\n\n\
        def main(stdout: Stdout):\n    shared log = list[int]()\n    shared a = Account()\n    \
        lock log as ref lines:\n        lines.append(1)\n        outer(a)\n";
    assert_eq!(locking(around_it), ["`outer` takes `account`'s lock while `lines` is locked @ outer(a)"]);
}

/// A nested function that calls a function value from around it does what the value does: an
/// arrow function's own body, or what the caller passes for a parameter. Called inside a lock, it's
/// refused for that, naming what the value waits on or locks, and one whose value does neither
/// stays fine. Only a value it can't follow, as one held in a list, counts for all the function
/// around it does. One with nothing of the kind, as an interface's method of the same name, stays
/// fine.
#[test]
fn a_nested_function_calling_a_captured_value_takes_what_it_takes() {
    let empty: [&str; 0] = [];
    let waits = "struct Scores:\n    total = 0\n\ndef nap(clock: Clock):\n    clock.sleep(1s)\n\n\
        def serve(stdout: Stdout, clock: Clock):\n    shared scores = Scores()\n    resting = () => nap(clock)\n    \
        def later():\n        resting()\n    lock scores as held:\n        later()\n        _ = held\n";
    assert_eq!(locking(waits), ["`later` waits on `Clock.sleep` while `held` is locked @ later()"]);
    let locks = "def read(account: shared Account) -> int:\n    lock account as now:\n        return now.total\n\n\
        def serve(stdout: Stdout):\n    shared scores = Account()\n    shared others = Account()\n    \
        peek = () => read(others)\n    def later() -> int:\n        return peek()\n    \
        lock scores as ref held:\n        held.total = later()\n";
    // The lock the arrow's body takes, not the one the function around it does.
    assert_eq!(locking(locks), ["`later` takes `account`'s lock while `held` is locked @ later()"]);
    let harmless = "def serve(stdout: Stdout):\n    shared scores = Account()\n    shown = () => stdout.print(1)\n    \
        def later():\n        shown()\n    lock scores as held:\n        later()\n        _ = held\n";
    assert_eq!(locking(harmless), empty);
    // Through a nested function that calls another, however deep.
    let deeper = "def nap(clock: Clock):\n    clock.sleep(1s)\n\n\
        def serve(stdout: Stdout, clock: Clock):\n    shared scores = Account()\n    resting = () => nap(clock)\n    \
        def inner():\n        resting()\n    def outer():\n        inner()\n    \
        lock scores as held:\n        outer()\n        _ = held\n";
    assert_eq!(locking(deeper), ["`outer` waits on `Clock.sleep` while `held` is locked @ outer()"]);
    let parameter = "def nap(clock: Clock):\n    clock.sleep(1s)\n\n\
        def guarded(scores: shared Account, work: () -> nothing):\n    def step():\n        work()\n    \
        lock scores as held:\n        step()\n        _ = held\n\n\
        def serve(stdout: Stdout, clock: Clock):\n    shared scores = Account()\n    guarded(scores, () => nap(clock))\n";
    // The parameter is followed to what the caller passes, and the nested function takes no lock
    // of the function around it.
    assert_eq!(
        locking(parameter),
        ["`guarded` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ () => nap(clock)"]
    );
    // Called inside the lock through another nested function.
    let chain = "def nap(clock: Clock):\n    clock.sleep(1s)\n\n\
        def guarded(scores: shared Account, work: () -> nothing):\n    def middle():\n        def inner():\n            work()\n        inner()\n    \
        lock scores as held:\n        middle()\n        _ = held\n\n\
        def serve(stdout: Stdout, clock: Clock):\n    shared scores = Account()\n    guarded(scores, () => nap(clock))\n";
    assert_eq!(
        locking(chain),
        ["`guarded` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ () => nap(clock)"]
    );
    let interface = "struct Scores:\n    total = 0\n\ninterface Counter:\n    def count(self) -> int\n\n\
        struct Plain:\n    n = 0\n\n    def count(self) -> int:\n        return self.n\n\n\
        def through[T: Counter](counter: T, scores: shared Scores) -> int:\n    lock scores as held:\n        \
        return held.total + counter.count()\n\n\
        def serve(stdout: Stdout, clock: Clock):\n    shared scores = Scores()\n    def count():\n        \
        clock.sleep(1s)\n    count()\n    stdout.print(through(Plain(), scores))\n";
    assert_eq!(locking(interface), empty);
    let in_a_list = "struct Scores:\n    total = 0\n\ndef nap(clock: Clock):\n    clock.sleep(1s)\n\n\
        def serve(stdout: Stdout, clock: Clock):\n    shared scores = Scores()\n    works = [() => nap(clock)]\n    \
        def later():\n        for work in works:\n            work()\n    lock scores as held:\n        later()\n        _ = held\n";
    assert_eq!(locking(in_a_list), ["`later` waits on `Clock.sleep` while `held` is locked @ later()"]);
}

const NAP: &str = "def nap(clock: Clock):\n    clock.sleep(1s)\n\n";
const RUN: &str = "def run(job: () -> nothing):\n    job()\n\n";
const APPLY: &str = "def apply(scores: shared Account, job: () -> nothing):\n    lock scores as held:\n        job()\n        _ = held\n\n";
/// A `main` that gives `guarded` an arrow function that sleeps.
const GUARDED_MAIN: &str =
    "def main(clock: Clock):\n    shared scores = Account()\n    guarded(scores, () => nap(clock))\n";
/// What refusing a program that calls `guarded`'s sleeping `work` inside `held` says.
const WAITS: [&str; 1] =
    ["`guarded` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ () => nap(clock)"];

/// What's refused in a program that waits, or takes a second lock, inside a lock: it has to be.
fn refused_in_a_lock(program: &str) -> Vec<String> {
    let problems = locking(program);
    assert!(!problems.is_empty(), "built, though it waits inside a lock:\n{program}");
    problems
}

/// Each way a function value from around a nested function can reach it, or the nested function
/// reach a lock, that following the value precisely could miss: called inside a lock, each waits,
/// so each is refused, as the checker lane's review of #2109 found they weren't.
#[test]
fn a_nested_function_is_refused_however_a_captured_value_reaches_it() {
    // A `mut` local given an arrow function after another value: at run time it holds `work`.
    let reassigned = format!(
        "{NAP}def guarded(stdout: Stdout, scores: shared Account, work: () -> nothing, quick: bool):\n    \
         mut job = work\n    if quick:\n        job = () => stdout.print(1)\n    def later():\n        job()\n    \
         lock scores as held:\n        later()\n        _ = held\n\n\
         def main(stdout: Stdout, clock: Clock):\n    shared scores = Account()\n    \
         guarded(stdout, scores, () => nap(clock), False)\n"
    );
    assert_eq!(
        refused_in_a_lock(&reassigned),
        ["`later` waits on a function value that can't be followed while `held` is locked @ later()"]
    );
    // The same through a loop, given the arrow that waits after the nested function is written.
    let in_a_loop = format!(
        "{NAP}def quiet(clock: Clock):\n    pass\n\n\
         def serve(clock: Clock):\n    shared scores = Account()\n    mut job = () => quiet(clock)\n    \
         mut rounds = 0\n    while rounds < 2:\n        def later():\n            job()\n        \
         lock scores as held:\n            later()\n            _ = held\n        job = () => nap(clock)\n        \
         rounds += 1\n"
    );
    assert_eq!(
        refused_in_a_lock(&in_a_loop),
        ["`later` waits on `Clock.sleep` while `held` is locked @ later()"]
    );
    // Calling the `mut` local inside the lock itself can't be followed either.
    let called_there = format!(
        "{NAP}def quiet(clock: Clock):\n    pass\n\n\
         def serve(clock: Clock):\n    shared scores = Account()\n    mut job = () => quiet(clock)\n    \
         mut rounds = 0\n    while rounds < 2:\n        lock scores as held:\n            job()\n            _ = held\n        \
         job = () => nap(clock)\n        rounds += 1\n"
    );
    assert_eq!(
        refused_in_a_lock(&called_there),
        [
            "what `job` holds can't be known here, so it can't be shown not to wait while `held` is locked @ job()"
        ]
    );
    // A captured parameter handed on.
    let handed_on = format!(
        "{NAP}{RUN}def guarded(scores: shared Account, work: () -> nothing):\n    def step():\n        run(work)\n    \
         lock scores as held:\n        step()\n        _ = held\n\n{GUARDED_MAIN}"
    );
    assert_eq!(refused_in_a_lock(&handed_on), WAITS);
    // A captured parameter given to a local, called in an arrow function, or held by an arrow.
    for used in [
        "        job = work\n        job()\n",
        "        run(() => work())\n",
        "        again = () => work()\n        again()\n",
    ] {
        let program = format!(
            "{NAP}{RUN}def guarded(scores: shared Account, work: () -> nothing):\n    def step():\n{used}    \
             lock scores as held:\n        step()\n        _ = held\n\n{GUARDED_MAIN}"
        );
        assert_eq!(refused_in_a_lock(&program), WAITS, "{used}");
    }
    // A nested function calling a captured parameter, passed by name inside the lock.
    let by_name = format!(
        "{NAP}{RUN}def guarded(scores: shared Account, work: () -> nothing):\n    def step():\n        work()\n    \
         lock scores as held:\n        run(step)\n        _ = held\n\n{GUARDED_MAIN}"
    );
    assert_eq!(refused_in_a_lock(&by_name), WAITS);
    // An arrow function calling an interface's method, which a type's method that waits gives.
    let interface = format!(
        "{NAP}interface Napper:\n    def nap(self, clock: Clock)\n\n\
         struct Slow:\n    held = 0\n\n    def nap(self, clock: Clock):\n        clock.sleep(1s)\n\n\
         def serve[T: Napper](napper: T, clock: Clock):\n    shared scores = Account()\n    \
         resting = () => napper.nap(clock)\n    def later():\n        resting()\n    \
         lock scores as held:\n        later()\n        _ = held\n\n\
         def main(clock: Clock):\n    serve(Slow(), clock)\n"
    );
    assert_eq!(
        refused_in_a_lock(&interface),
        ["`later` waits on `Clock.sleep` while `held` is locked @ later()"]
    );
}

/// The second pass of the checker lane's review of #2109: a parameter that holds function values
/// without being one, a nested function calling a captured parameter reached through an arrow
/// function called inside a lock, and a name declared again after a block gave it an arrow.
#[test]
fn a_nested_function_is_refused_through_an_arrow_or_a_name_declared_again() {
    // A captured parameter that holds function values without being one, as a list of them, can't
    // be followed: the nested function counts for all its function does.
    let in_a_list = format!(
        "{NAP}def guarded(scores: shared Account, handlers: list[() -> nothing]):\n    def step():\n        \
         handlers[0]()\n    lock scores as held:\n        step()\n        _ = held\n\n\
         def main(clock: Clock):\n    shared scores = Account()\n    guarded(scores, [() => nap(clock)])\n"
    );
    assert_eq!(
        refused_in_a_lock(&in_a_list),
        ["`step` waits on a function value that can't be followed while `held` is locked @ step()"]
    );
    // A nested function calling a captured parameter, reached through an arrow function called
    // inside a lock: passed for a parameter called inside one, or held by a local name.
    for reached in [
        "    apply(scores, () => step())\n",
        "    go = () => step()\n    lock scores as held:\n        go()\n        _ = held\n",
    ] {
        let program = format!(
            "{NAP}{APPLY}def guarded(scores: shared Account, work: () -> nothing):\n    def step():\n        \
             work()\n{reached}\n{GUARDED_MAIN}"
        );
        assert_eq!(refused_in_a_lock(&program), WAITS, "{reached}");
    }
    // The same with no nested function: an arrow function handing the parameter on.
    let handed_in_an_arrow = format!(
        "{NAP}{APPLY}{RUN}def guarded(scores: shared Account, work: () -> nothing):\n    \
         apply(scores, () => run(work))\n\n{GUARDED_MAIN}"
    );
    assert_eq!(refused_in_a_lock(&handed_in_an_arrow), WAITS);
    // A name given an arrow function in a block, then declared again after it, by unpacking: the
    // arrow went with the block, and the name holds `work`.
    for (called, said) in [
        (
            "    def later():\n        job()\n",
            "`later` waits on a function value that can't be followed while `held` is locked @ later()",
        ),
        (
            "",
            "what `job` holds can't be known here, so it can't be shown not to wait while `held` is locked @ job()",
        ),
    ] {
        let calls = if called.is_empty() { "job()" } else { "later()" };
        let program = format!(
            "{NAP}def guarded(stdout: Stdout, scores: shared Account, work: () -> nothing, quick: bool):\n    \
             if quick:\n        job = () => stdout.print(1)\n        job()\n    job, count = (work, 1)\n    \
             stdout.print(count)\n{called}    lock scores as held:\n        {calls}\n        _ = held\n\n\
             def main(stdout: Stdout, clock: Clock):\n    shared scores = Account()\n    \
             guarded(stdout, scores, () => nap(clock), False)\n"
        );
        assert_eq!(refused_in_a_lock(&program), [said]);
    }
}

/// An optional function parameter is followed as a function parameter is, once it's narrowed:
/// called inside a lock, what each caller passes for it is judged there, and `None` is nothing to
/// judge. So is one a nested function captures and calls, narrowed first. It's written through an
/// alias, as `(() -> nothing)?` can't be written yet (#2132).
#[test]
fn an_optional_function_parameter_is_followed_once_narrowed() {
    let empty: [&str; 0] = [];
    let guarded = |body: &str, passed: &str| {
        format!(
            "{NAP}type Job = () -> nothing\n\ndef guarded(scores: shared Account, work: Job?):\n    \
             if work is not None:\n{body}\n\
             def main(clock: Clock, stdout: Stdout):\n    shared scores = Account()\n    guarded(scores, {passed})\n"
        )
    };
    let in_the_lock = "        lock scores as held:\n            work()\n            _ = held\n";
    assert_eq!(refused_in_a_lock(&guarded(in_the_lock, "() => nap(clock)")), WAITS);
    assert_eq!(locking(&guarded(in_the_lock, "None")), empty);
    assert_eq!(locking(&guarded(in_the_lock, "() => stdout.print(1)")), empty);
    let captured = "        def step():\n            work()\n        lock scores as held:\n            step()\n            \
                    _ = held\n";
    assert_eq!(refused_in_a_lock(&guarded(captured, "() => nap(clock)")), WAITS);
    assert_eq!(locking(&guarded(captured, "None")), empty);
}

/// The checker lane's review of #2139: a function parameter read any way but a call, an argument
/// for a function parameter, or an `is None` test counts as called right there. Unwrapped into
/// another name, kept in a list, returned or handed to an interface's method, what each caller
/// passes is judged where it's passed: one that waits is refused inside a lock, and one that
/// doesn't is let through.
#[test]
fn a_function_parameter_read_any_other_way_counts_as_called() {
    let empty: [&str; 0] = [];
    let in_the_lock = |helper: &str, passed: &str, call: &str| {
        format!(
            "{NAP}type Job = () -> nothing\n\n{helper}\ndef main(clock: Clock, stdout: Stdout):\n    \
             shared scores = Account()\n    rest = {passed}\n    shelf = Shelf()\n    \
             lock scores as held:\n        {call}\n        _ = held\n\n\
             interface Keeper:\n    def keep(self, job: Job)\n\n\
             struct Shelf:\n    count = 0\n\n    def keep(self, job: Job):\n        pass\n"
        )
    };
    let waits = ["`guarded` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ rest"];
    for (helper, call) in [
        // Given to another name, then called through it: a plain one, and, the review's program,
        // an optional one unwrapped.
        ("def guarded(work: Job):\n    job = work\n    job()\n", "guarded(rest)"),
        ("def guarded(work: Job?):\n    job = work else return\n    job()\n", "guarded(rest)"),
        ("def guarded(work: Job) -> list[Job]:\n    return [work]\n", "_ = guarded(rest)"),
        ("def guarded(work: Job) -> Job:\n    return work\n", "_ = guarded(rest)"),
        ("def guarded[T: Keeper](keeper: T, work: Job):\n    keeper.keep(work)\n", "guarded(shelf, rest)"),
    ] {
        assert_eq!(refused_in_a_lock(&in_the_lock(helper, "() => nap(clock)", call)), waits, "{helper}");
        assert_eq!(locking(&in_the_lock(helper, "() => stdout.print(1)", call)), empty, "{helper}");
    }
    // Asking whether there's a function calls none.
    let tested = "def guarded(work: Job?) -> bool:\n    return work is not None\n";
    assert_eq!(locking(&in_the_lock(tested, "() => nap(clock)", "_ = guarded(rest)")), empty);
}

/// #2113: what a function value does reaches a lock however it's held where it's called. Each
/// program waits inside a lock, so each is refused.
#[test]
fn a_function_value_waits_inside_a_lock_however_it_is_held() {
    // Each helper is called inside the lock, with a value made before it.
    let lock_around = |helper: &str, made: &str, call: &str| {
        format!(
            "{NAP}{helper}\ndef main(clock: Clock):\n    shared scores = Account()\n    {made}\n    \
             lock scores as held:\n        {call}\n        _ = held\n"
        )
    };
    // A parameter given to a local name, then called through it.
    let aliased = lock_around(
        "def helper(work: () -> nothing):\n    job = work\n    job()\n",
        "resting = () => nap(clock)",
        "helper(resting)",
    );
    assert_eq!(
        refused_in_a_lock(&aliased),
        ["`helper` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ resting"]
    );
    // A function value called from a list, outside the lock.
    let listed = lock_around(
        "def helper(jobs: list[() -> nothing]):\n    jobs[0]()\n",
        "jobs = [() => nap(clock)]",
        "helper(jobs)",
    );
    assert_eq!(
        refused_in_a_lock(&listed),
        ["`helper` waits on a function value that can't be followed while `held` is locked @ helper(jobs)"]
    );
    // An interface's method called on a type parameter, outside the lock.
    let interface = lock_around(
        "interface Napper:\n    def nap(self, clock: Clock)\n\n\
         struct Slow:\n    held = 0\n\n    def nap(self, clock: Clock):\n        clock.sleep(1s)\n\n\
         def helper[T: Napper](napper: T, clock: Clock):\n    napper.nap(clock)\n",
        "slow = Slow()",
        "helper(slow, clock)",
    );
    assert_eq!(
        refused_in_a_lock(&interface),
        ["`helper` waits on `Clock.sleep` while `held` is locked @ helper(slow, clock)"]
    );
    // A `ref` parameter given a sleeping arrow before a nested function calls it: what it's passed
    // is a place, which can't be known, so the call is refused there.
    let given_by_ref = format!(
        "{NAP}def quiet():\n    pass\n\n\
         def guarded(scores: shared Account, ref work: () -> nothing, clock: Clock):\n    work = () => nap(clock)\n    \
         def step():\n        work()\n    lock scores as held:\n        step()\n        _ = held\n\n\
         def main(clock: Clock):\n    shared scores = Account()\n    mut job = () => quiet()\n    \
         guarded(scores, ref job, clock)\n"
    );
    assert_eq!(
        refused_in_a_lock(&given_by_ref),
        [
            "`guarded` calls `work` while `held` is locked, and what this holds can't be known, so it can't be shown not to wait @ job"
        ]
    );
}

/// The third pass: a captured arrow function that hands a parameter on. (Its other program, an
/// optional function parameter narrowed before the nested function, can't be written yet:
/// `(() -> nothing)?` is an optional tuple. `captured_values` reads a parameter's declared type
/// all the same, so narrowing never makes one followed.)
#[test]
fn a_nested_function_is_refused_through_an_arrow_handing_a_parameter_on() {
    // A captured arrow function that hands `work` on to one that calls it, the nested function
    // reading it called inside the lock, or passed by name inside it.
    for called in ["later()", "run(later)"] {
        let program = format!(
            "{NAP}{RUN}def guarded(scores: shared Account, work: () -> nothing):\n    go = () => run(work)\n    \
             def later():\n        go()\n    lock scores as held:\n        {called}\n        _ = held\n\n\
             {GUARDED_MAIN}"
        );
        assert_eq!(refused_in_a_lock(&program), WAITS, "{called}");
    }
}

const HOLDERS: &str = "struct Cell:
    count = 0

    def add(ref self, by: int):
        self.count += by

struct Holder:
    cell: shared Cell

    def poke(self):
        lock self.cell as ref held:
            held.add(1)

def through(holder: Holder):
    holder.poke()

def through_all(cells: list[shared Cell]):
    lock cells[0] as ref held:
        held.add(1)

def plain(n: int) -> int:
    return n + 1

def apply(work: (int) -> int) -> int:
    return work(1)
";

fn in_a_lock(body: &str) -> Vec<String> {
    let body = body.replace('\n', "\n    ");
    let found = crate::in_main_after(
        HOLDERS,
        &format!("shared one = Cell()\nlock one as ref locked:\n    locked.count += 1\n    {body}\n"),
    );
    found.iter().filter_map(|said| said.split(" @ ").next().map(str::to_owned)).collect()
}

/// A call under a lock that takes another, however many calls down, is refused, whether the
/// `shared` value is handed to it inside something else or reached through what it holds.
#[test]
fn what_takes_another_lock_cant_be_called_under_one() {
    assert_eq!(
        in_a_lock("through(Holder(cell=one))"),
        ["`through` takes `self.cell`'s lock while `locked` is locked"]
    );
    assert_eq!(in_a_lock("through_all([one])"), ["`through_all` takes a lock while `locked` is locked"]);
    assert_eq!(
        in_a_lock("holder = Holder(cell=one)\nholder.poke()"),
        ["`poke` takes `self.cell`'s lock while `locked` is locked"]
    );
}

/// A nested function that locks a `shared` value it reads is refused under that lock, called
/// directly or through another.
#[test]
fn a_function_that_locks_what_it_captures_cant_run_under_a_lock() {
    let before = |lines: &str, body: &str| {
        let lines = lines.replace('\n', "\n    ");
        let source = format!(
            "def main(stdout: Stdout):\n    shared one = Cell()\n    {lines}\n    lock one as ref locked:\n        locked.count += 1\n        {body}\n"
        );
        problems(&format!("{HOLDERS}\n{source}"))
            .iter()
            .filter_map(|said| said.split(" @ ").next().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let nested = "def poke():\n    lock one as ref held:\n        held.add(1)";
    assert_eq!(before(nested, "poke()"), ["`poke` takes `one`'s lock while `locked` is locked"]);
    let through = "def poke():\n    lock one as ref held:\n        held.add(1)\ndef again():\n    poke()";
    assert_eq!(before(through, "again()"), ["`again` takes `one`'s lock while `locked` is locked"]);
    // An arrow function given to a name captures it as a nested function does.
    let arrow = "poke = () => through(Holder(cell=one))";
    assert_eq!(
        before(arrow, "poke()"),
        ["`poke` is called while `locked` is locked, and this takes `self.cell`'s lock"]
    );
}

#[test]
fn what_takes_no_other_lock_goes_on_as_before_under_a_lock() {
    let empty: [&str; 0] = [];
    for body in [
        "locked.count += apply(n => n + 1)",
        "numbers = [1]\nlocked.count += len(numbers)",
        "locked.count += plain(1)",
        // Making a value that holds it hands it to no call.
        "holder = Holder(cell=one)\nlocked.count += 1",
    ] {
        assert_eq!(in_a_lock(body), empty, "{body}");
    }
    let outside = "shared one = Cell()\nholder = Holder(cell=one)\nthrough(holder)\nholder.poke()\n";
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
    let errors = "error Oops:\n    why: str\n\ndef risky() -> int fails Oops:\n    return 1\n\n";
    let empty: [&str; 0] = [];
    let good = format!(
        "{errors}def opened(name: str) -> with str fails Oops:\n    _ = risky()?\n    lend name + \"!\" except:\n        pass\n\ndef sized(name: str) -> int fails Oops:\n    with opened(name)? as text:\n        return len(text)\n    return 0\n"
    );
    assert_eq!(problems(&good), empty);
    // A last line after a block that always returns still takes the return type.
    let after = format!(
        "{errors}def opened(name: str) -> with str fails Oops:\n    lend name\n\ndef named(name: str) -> list[str] fails Oops:\n    with opened(name)? as text:\n        return [text]\n    return []\n"
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
        ["a `?` that might not run isn't built yet in a source's block"]
    );
}

/// Every way through a source that doesn't fail reaches its `lend`, since its caller's block runs
/// there: a source that could finish without lending would skip the block with nothing said, as
/// `maybe` once did with `flag` false, printing what came after its caller's `with`. A way that
/// fails is fine, since the caller's `with` fails with it.
#[test]
fn a_with_source_that_can_finish_without_lending_is_refused() {
    let errors = "error Oops:\n    code: int\n\nenum Kind:\n    Lent\n    Failed\n\n";
    let empty: [&str; 0] = [];
    let unlent = |name: &str| {
        format!(
            "`{name}` can finish without lending: every way through a `with` source that doesn't fail reaches its `lend`"
        )
    };
    let found = |body: &str| -> Vec<String> {
        problems(&format!("{errors}{body}"))
            .iter()
            .map(|problem| problem.split(" @ ").next().unwrap_or_default().to_owned())
            .collect()
    };
    let skipped = "def maybe(flag: bool) -> with int:\n    if flag:\n        lend 1\n\n\
        def run(flag: bool, stdout: Stdout) fails Oops:\n    with maybe(flag) as n:\n        fail Oops(code=n)\n    stdout.print(\"skipped\")\n";
    assert_eq!(found(skipped), [unlent("maybe")]);
    assert_eq!(
        helps(&format!("{errors}{skipped}")),
        [
            "on a way that shouldn't lend, `fail` with an error, so the caller's `with` fails rather than skipping its block"
        ]
    );
    // An `elif` that neither lends nor fails is a way through too, and so is a `match` over an int
    // that no case takes.
    assert_eq!(
        found(
            "def picked(n: int) -> with int fails Oops:\n    if n > 0:\n        lend n\n    elif n < 0:\n        fail Oops(code=n)\n    elif n == 0:\n        pass\n    else:\n        fail Oops(code=1)\n"
        ),
        [unlent("picked")]
    );
    assert_eq!(
        found(
            "def cased(n: int) -> with int fails Oops:\n    match n:\n        case 1:\n            lend 1\n        case 2:\n            fail Oops(code=2)\n"
        ),
        [unlent("cased")]
    );
    // Every way that doesn't fail lends: the other branch fails, a `match` over an enum takes every
    // case, the `lend` comes after an `if` that doesn't leave, or inside a `with` block, which runs
    // once its source's call works. A `?` before the `lend` fails the way it's on.
    assert_eq!(
        found(
            "def sure(flag: bool) -> with int fails Oops:\n    if flag:\n        lend 1\n    else:\n        fail Oops(code=0)\n"
        ),
        empty
    );
    assert_eq!(
        found(
            "def chosen(kind: Kind) -> with int fails Oops:\n    match kind:\n        case .Lent:\n            lend 1\n        case .Failed:\n            fail Oops(code=2)\n"
        ),
        empty
    );
    assert_eq!(
        found("def after(flag: bool) -> with int:\n    mut n = 0\n    if flag:\n        n = 1\n    lend n\n"),
        empty
    );
    assert_eq!(
        found(
            "def inner() -> with int:\n    lend 1\n\ndef outer() -> with int:\n    with inner() as n:\n        lend n + 1\n"
        ),
        empty
    );
    assert_eq!(
        found(
            "def checked(code: int) -> int fails Oops:\n    if code > 0: fail Oops(code=code)\n    return code\n\n\
             def guarded(code: int) -> with int fails Oops:\n    n = checked(code)?\n    lend n\n"
        ),
        empty
    );
    // A way that never ends doesn't finish without lending.
    assert_eq!(
        found(
            "def forever(flag: bool) -> with int:\n    if flag:\n        lend 1\n    else:\n        while True:\n            pass\n"
        ),
        empty
    );
    // A `lend` in each branch is still two `lend`s, refused as that alone.
    assert_eq!(
        found("def each(flag: bool) -> with int:\n    if flag:\n        lend 1\n    else:\n        lend 2\n"),
        ["`each` lends twice, but a `with` source lends once"]
    );
}

/// A source's `else:` that fails with a case written with a leading dot, `.Stuck(...)`, is
/// written in where it's used with its enum named, as a note on the block's failure, where no
/// expected type would say which enum it's from.
#[test]
fn a_failing_else_with_a_leading_dot_case_is_written_in() {
    let source = "error Shut:\n    Stuck(name: str)\n    Jammed\n\ndef opened(name: str) -> with str fails Shut:\n    lend name except:\n        fail .Stuck(name=name)\n\ndef single() fails Shut:\n    with opened(\"single\")? as door:\n        fail .Jammed\n\ndef main(stdout: Stdout):\n    single() except why:\n        stdout.print(\"failed\", why, why.notes())\n        return\n    stdout.print(\"worked\")\n";
    let mut module = fennec_syntax::parse(source).unwrap();
    let resolved = fennec_check::check_and_resolve(&mut module);
    assert!(resolved.is_ok(), "{:?}", resolved.err());
}

/// One `with` opens several handles, each with its name, and a source that lends a tuple
/// splits it into a name each: `with conn.split() as reader, writer:`.
#[test]
fn a_with_opens_several_handles_and_splits_a_lent_tuple() {
    let empty: [&str; 0] = [];
    let sources = "error Oops:\n    why: str\n\n\
        def opened(name: str) -> with str fails Oops:\n    lend name\n\n\
        def pair() -> with (int, str):\n    lend (1, \"a\")\n\n";
    let several = format!(
        "{sources}def main(stdout: Stdout) fails Oops:\n    with opened(\"a\")? as a, opened(\"b\")? as b, pair() as both:\n        stdout.print(a + b, both)\n"
    );
    assert_eq!(problems(&several), empty);
    let split = format!(
        "{sources}def main(stdout: Stdout):\n    with pair() as number, text:\n        stdout.print(number + 1, text + \"!\")\n"
    );
    assert_eq!(problems(&split), empty);
    let wrong = format!(
        "{sources}def main(stdout: Stdout) fails Oops:\n    with opened(\"a\")? as x, y:\n        pass\n"
    );
    assert_eq!(
        problems(&wrong),
        ["this `with` names 2 values, but what it takes lends a str @ opened(\"a\")?"]
    );
    let counted = format!("{sources}def main(stdout: Stdout):\n    with pair() as x, y, z:\n        pass\n");
    assert_eq!(
        problems(&counted),
        ["this `with` names 3 values, but what it takes lends a (int, str) @ pair()"]
    );
}

/// What a source lends is the block's to use, `ref self` methods included: written `as ref` or
/// not, it only can't leave the block.
#[test]
fn a_lent_value_takes_ref_self_calls() {
    let empty: [&str; 0] = [];
    let program = "struct Sink:\n    parts = list[str]()\n\n    def write(ref self, text: str):\n        self.parts.append(text)\n\n\
        def sink() -> with Sink:\n    mut held = Sink()\n    lend held\n\n\
        def main(stdout: Stdout):\n    with sink() as out:\n        out.write(\"a\")\n    with sink() as ref out:\n        out.write(\"b\")\n";
    assert_eq!(problems(program), empty);
}

/// Nothing waits inside a lock, however many calls down: a task that pauses holding one stops
/// every task that wants it. Traced across the whole program, through plain functions, methods
/// and interfaces, while what doesn't wait, like printing or a plain function, is fine.
#[test]
fn nothing_waits_inside_a_lock() {
    let found = |body: &str| -> Vec<String> {
        problems(&format!(
            "from std.concurrent import Channel, Closed, Timeout\n\n\
             struct Scores:\n    total = 0\n\n\
             interface Napper:\n    def nap(self, clock: Clock)\n\n\
             struct Quick:\n    held = 0\n\n    def nap(self, clock: Clock):\n        pass\n\n\
             struct Slow:\n    held = 0\n\n    def nap(self, clock: Clock):\n        clock.sleep(1s)\n\n\
             def rest(clock: Clock):\n    clock.sleep(1s)\n\n\
             def deeper(clock: Clock):\n    rest(clock)\n\n\
             def bump(ref scores: Scores):\n    scores.total += 1\n\n\
             def ask(jobs: Channel[int]) -> int fails Closed | Timeout:\n    jobs.receive() else fail Closed()\n\n\
             def through[T: Napper](napper: T, clock: Clock, shared_scores: shared Scores):\n    \
             lock shared_scores as held:\n        napper.nap(clock)\n        _ = held\n\n\
             def serve(stdout: Stdout, clock: Clock, jobs: Channel[int]) fails Closed | Timeout:\n    \
             shared scores = Scores()\n    lock scores as ref held:\n{body}"
        ))
        .into_iter()
        .map(|problem| problem.split(" @ ").next().unwrap_or_default().to_owned())
        .filter(|problem| !problem.contains("`nap` waits"))
        .collect()
    };
    let empty: [&str; 0] = [];
    assert_eq!(found("        bump(ref held)\n        stdout.print(held.total)\n"), empty);
    assert_eq!(found("        clock.sleep(1s)\n"), ["`Clock.sleep` waits while `held` is locked"]);
    assert_eq!(found("        deeper(clock)\n"), ["`deeper` waits on `Clock.sleep` while `held` is locked"]);
    assert_eq!(
        found("        got = ask(jobs)?\n        held.total += got\n"),
        ["`ask` waits on a channel's `receive` while `held` is locked"]
    );
    assert_eq!(
        found("        got = jobs.receive() else fail Closed()\n        held.total += got\n"),
        ["`receive` waits on a channel's `receive` while `held` is locked"]
    );
    assert_eq!(
        found("        for n in parallel([1, 2]):\n            stdout.print(n)\n"),
        ["`parallel` waits while `held` is locked"]
    );
}

/// One lock is never taken inside another, however many calls down, so two tasks can't each hold
/// one the other wants: they're taken together in one `lock`.
#[test]
fn no_lock_is_taken_inside_another_through_a_call() {
    let found = |body: &str| -> Vec<String> {
        problems(&format!(
            "struct Scores:\n    total = 0\n\n\
             interface Counter:\n    def count(self) -> int\n\n\
             struct Locking:\n    scores: shared Scores\n\n    def count(self) -> int:\n        lock self.scores as held:\n            return held.total\n\n\
             def read(scores: shared Scores) -> int:\n    lock scores as held:\n        return held.total\n\n\
             def deeper(scores: shared Scores) -> int:\n    read(scores)\n\n\
             def plain(total: int) -> int:\n    total + 1\n\n\
             def through[T: Counter](counter: T, scores: shared Scores) -> int:\n    \
             lock scores as held:\n        return held.total + counter.count()\n\n\
             def main(stdout: Stdout):\n    shared scores = Scores()\n    shared others = Scores()\n    \
             stdout.print(through(Locking(scores=others), scores))\n    lock scores as ref held:\n{body}"
        ))
        .into_iter()
        .map(|problem| problem.split(" @ ").next().unwrap_or_default().to_owned())
        .filter(|problem| !problem.contains("`count` takes"))
        .collect()
    };
    let empty: [&str; 0] = [];
    assert_eq!(found("        held.total = plain(held.total)\n"), empty);
    // An interface's method could be any type's, so one that locks on any type counts.
    let all = problems(
        "struct Scores:\n    total = 0\n\ninterface Counter:\n    def count(self) -> int\n\n\
        struct Locking:\n    scores: shared Scores\n\n    def count(self) -> int:\n        lock self.scores as held:\n            return held.total\n\n\
        def through[T: Counter](counter: T, scores: shared Scores) -> int:\n    lock scores as held:\n        return held.total + counter.count()\n\n\
        def main(stdout: Stdout):\n    shared scores = Scores()\n    shared others = Scores()\n    stdout.print(through(Locking(scores=others), scores))\n",
    );
    let said: Vec<&str> = all.iter().filter_map(|problem| problem.split(" @ ").next()).collect();
    assert_eq!(said, ["`count` takes `self.scores`'s lock while `held` is locked"]);
    assert_eq!(
        found("        held.total = read(others)\n"),
        ["`read` takes `scores`'s lock while `held` is locked"]
    );
    assert_eq!(
        found("        held.total = deeper(others)\n"),
        ["`deeper` takes `scores`'s lock while `held` is locked"]
    );
}

/// A method of an interface could be any type's, so one that waits on any type counts.
#[test]
fn an_interface_method_inside_a_lock_counts_if_any_type_s_waits() {
    let program = "struct Scores:\n    total = 0\n\n\
        interface Napper:\n    def nap(self, clock: Clock)\n\n\
        struct Slow:\n    held = 0\n\n    def nap(self, clock: Clock):\n        clock.sleep(1s)\n\n\
        def through[T: Napper](napper: T, clock: Clock, scores: shared Scores):\n    \
        lock scores as held:\n        napper.nap(clock)\n        _ = held\n";
    let found = problems(program);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("`nap` waits on `Clock.sleep` while `held` is locked"), "{found:?}");
}

/// A type's function called on a type parameter, as `T.made(clock)`, could be any type's of that
/// name too, inside the lock or in a function called inside one, as the checker lane's review of
/// #2167 found it wasn't.
#[test]
fn a_type_parameter_s_function_inside_a_lock_counts_if_any_type_s_waits() {
    let program = |made: &str, through: &str| {
        format!(
            "struct Scores:\n    total = 0\n\n\
             interface Made:\n    def made(clock: Clock) -> int\n\n\
             struct Slow:\n    size = 0\n\n    def made(clock: Clock) -> int:\n{made}        return 1\n\n\
             def helper[T: Made](clock: Clock) -> int:\n    return T.made(clock)\n\n\
             def through[T: Made](scores: shared Scores, clock: Clock) -> int:\n    \
             lock scores as held:\n        return held.total + {through}\n"
        )
    };
    let sleeps = "        clock.sleep(1s)\n";
    let inside = problems(&program(sleeps, "T.made(clock)"));
    assert_eq!(inside.len(), 1, "{inside:?}");
    assert!(inside[0].starts_with("`made` waits on `Clock.sleep` while `held` is locked"), "{inside:?}");
    let called = problems(&program(sleeps, "helper[T](clock)"));
    assert_eq!(called.len(), 1, "{called:?}");
    assert!(called[0].starts_with("`helper` waits on `Clock.sleep` while `held` is locked"), "{called:?}");
    // No type's `made` waits, so neither is refused.
    let quiet: Vec<String> = Vec::new();
    assert_eq!(problems(&program("", "T.made(clock)")), quiet);
    assert_eq!(problems(&program("", "helper[T](clock)")), quiet);
}

/// A function value called inside a lock is followed to what it could hold. What each caller
/// passes for a function parameter that's called under a lock, directly or through another
/// function it's passed to, is checked where it's passed: an arrow function by its body, a named
/// function by what it calls. A function value whose source can't be known, like one from a list,
/// is refused where it's called.
#[test]
fn a_function_value_called_inside_a_lock_is_followed_to_what_it_holds() {
    let declared = "from std.concurrent import Closed, Sender, Timeout\n\n\
        struct Scores:\n    total = 0\n\n\
        def locked(scores: shared Scores, work: () -> nothing fails Closed | Timeout) fails Closed | Timeout:\n    \
        lock scores as held:\n        work()?\n        _ = held\n\n\
        def calls(work: () -> nothing):\n    work()\n\n\
        def handed(scores: shared Scores, work: () -> nothing):\n    lock scores as held:\n        calls(work)\n        _ = held\n\n\
        def quiet():\n    pass\n\n\
        def twice(run: (() -> nothing) -> nothing):\n    run(quiet)\n\n\
        def kept() fails Closed | Timeout:\n    pass\n\n\
        def nap(clock: Clock):\n    clock.sleep(1s)\n\n";
    let found = |body: &str| -> Vec<String> {
        problems(&format!(
            "{declared}def serve(stdout: Stdout, clock: Clock, sender: Sender[int]) fails Closed | Timeout:\n    shared scores = Scores()\n{body}"
        ))
    };
    let empty: [&str; 0] = [];
    assert_eq!(
        found(
            "    handed(scores, () => stdout.print(1))\n    handed(scores, quiet)\n    locked(scores, kept)?\n"
        ),
        empty
    );
    assert_eq!(
        found("    locked(scores, () => sender.send(1)?)?\n"),
        [
            "`locked` calls `work` while `held` is locked, and this waits on a channel's `send` @ () => sender.send(1)?"
        ]
    );
    assert_eq!(
        found("    handed(scores, () => nap(clock))\n"),
        ["`handed` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ () => nap(clock)"]
    );
    assert_eq!(
        found("    lock scores as held:\n        calls(() => nap(clock))\n        _ = held\n"),
        ["`nap` waits on `Clock.sleep` while `held` is locked @ nap(clock)"]
    );
    assert_eq!(
        found("    twice(calls)\n"),
        [
            "`calls` calls the function it's given as `work`, so it's called only by name, where what it's given can be checked @ calls"
        ]
    );
    assert_eq!(
        found(
            "    resting = () => nap(clock)\n    lock scores as held:\n        resting()\n        _ = held\n"
        ),
        ["`resting` is called while `held` is locked, and this waits on `Clock.sleep` @ resting()"]
    );
    let each = "struct Scores:\n    total = 0\n\ndef each(scores: shared Scores, works: list[() -> nothing]):\n    \
        for work in works:\n        lock scores as held:\n            work()\n            _ = held\n";
    assert_eq!(
        problems(each),
        [
            "what `work` holds can't be known here, so it can't be shown not to wait while `held` is locked @ work()"
        ]
    );
}

/// A function parameter a nested function calls is called by the function around it: what each
/// caller passes is checked as when that function calls it itself. `runs` calling `work` through
/// its nested `step` once let `nap` sleep while `held` was locked, where `runs_plain`, calling it
/// itself, was refused.
#[test]
fn a_parameter_a_nested_function_calls_is_followed_as_its_function_calls_it() {
    let declared = "struct Account:\n    total = 0\n\n\
        def nap(clock: Clock):\n    clock.sleep(1s)\n\n\
        def quiet():\n    pass\n\n\
        def runs(work: () -> nothing):\n    def step():\n        work()\n    step()\n\n\
        def runs_plain(work: () -> nothing):\n    work()\n\n\
        def deeper(work: () -> nothing):\n    def outer():\n        def inner():\n            work()\n        inner()\n    outer()\n\n\
        def shadows(work: () -> nothing):\n    def step(work: () -> nothing):\n        work()\n    step(quiet)\n\n";
    let found = |body: &str| -> Vec<String> {
        problems(&format!(
            "{declared}def main(stdout: Stdout, clock: Clock):\n    shared scores = Account()\n    resting = () => nap(clock)\n{body}"
        ))
    };
    let empty: [&str; 0] = [];
    assert_eq!(
        found(
            "    lock scores as held:\n        runs(resting)\n        stdout.print(held.total)\n    lock scores as again:\n        runs_plain(resting)\n        stdout.print(again.total)\n"
        ),
        [
            "`runs` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ resting",
            "`runs_plain` calls `work` while `again` is locked, and this waits on `Clock.sleep` @ resting"
        ]
    );
    // However deep it's nested, and whoever's around it, it's the outer function's parameter.
    assert_eq!(
        found("    lock scores as held:\n        deeper(resting)\n        stdout.print(held.total)\n"),
        ["`deeper` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ resting"]
    );
    // What waits on nothing is fine, and a nested function's own parameter of the same name isn't
    // the outer one's.
    assert_eq!(
        found(
            "    lock scores as held:\n        runs(quiet)\n        runs(() => stdout.print(1))\n        shadows(resting)\n        stdout.print(held.total)\n"
        ),
        empty
    );
    // The nested function called inside a lock its function takes calls the parameter inside it.
    let inside = "struct Account:\n    total = 0\n\n\
        def nap(clock: Clock):\n    clock.sleep(1s)\n\n\
        def counted(scores: shared Account, work: () -> nothing):\n    def step():\n        work()\n    \
        lock scores as held:\n        step()\n        _ = held\n\n\
        def main(clock: Clock):\n    shared scores = Account()\n    counted(scores, () => nap(clock))\n";
    assert_eq!(
        problems(inside),
        ["`counted` calls `work` while `held` is locked, and this waits on `Clock.sleep` @ () => nap(clock)"]
    );
}

/// A source can't lend while it holds a lock it took: the block lent to runs inside the lock.
#[test]
fn a_source_cant_lend_inside_a_lock() {
    let found = problems(
        "struct Scores:\n    total = 0\n\ndef held(scores: shared Scores) -> with int:\n    lock scores as current:\n        lend current.total\n",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("a `lend` runs its caller's block, which can wait, while `current` is locked"),
        "{found:?}"
    );
}

/// A task, or a `shared` value, can't take a generator yet: what a task reads from around it, and
/// what a `shared` value holds, is kept where tasks can reach it, and a generator can't be kept
/// there. One made inside the task is fine, and so is a list of its items (#2189).
#[test]
fn a_generator_isnt_handed_to_a_task_or_a_shared_value_yet() {
    let numbers = "from std.concurrent import Crashed\n\n\
        def numbers() -> Iterator[int]:\n    yield 1\n    yield 2\n\n\
        def count(items: Iterator[int], stdout: Stdout):\n    stdout.print(sum(items))\n\n\
        def total(items: list[int], stdout: Stdout):\n    stdout.print(sum(items))\n\n\
        struct Holder:\n    items: Iterator[int]\n\n\
        struct Kept:\n    items: list[int]\n\n";
    let help = "keep its items with `list(...)` first, as `items = list(made)`, and hand on the list, or make the generator where it's used";
    let taken = format!(
        "{numbers}def main(stdout: Stdout) fails Crashed:\n    made = numbers()\n    with launch(() => count(made, stdout))?:\n        pass\n"
    );
    assert_eq!(
        problems(&taken),
        ["a task can't take `made`, which holds a generator, from around it yet @ made"]
    );
    assert_eq!(helps(&taken), [help]);
    let in_a_struct = format!(
        "{numbers}def main(stdout: Stdout) fails Crashed:\n    held = Holder(items=numbers())\n    with launch(() => count(held.items, stdout))?:\n        pass\n"
    );
    assert_eq!(
        problems(&in_a_struct),
        ["a task can't take `held`, which holds a generator, from around it yet @ held"]
    );
    let shared = format!(
        "{numbers}def main(stdout: Stdout):\n    shared holder = Holder(items=numbers())\n    lock holder as held:\n        stdout.print(sum(held.items))\n"
    );
    assert_eq!(problems(&shared), ["a `shared` value can't hold a generator yet @ Holder(items=numbers())"]);
    let fine = format!(
        "{numbers}def main(stdout: Stdout) fails Crashed:\n    items = list(numbers())\n    with launch(() => count(numbers(), stdout))?:\n        pass\n    with launch(() => total(items, stdout))?:\n        pass\n    shared kept = Kept(items=list(numbers()))\n    lock kept as held:\n        stdout.print(sum(held.items))\n"
    );
    assert_eq!(problems(&fine), Vec::<String>::new());
}
