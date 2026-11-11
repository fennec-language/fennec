//! `shared T`: a value many tasks reach, each inside a `lock`, which replaced `shared Name:`
//! declarations and `with` on them.

use crate::problems;

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
    n + 1

def apply(work: (int) -> int) -> int:
    work(1)
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
}

#[test]
fn what_takes_no_other_lock_goes_on_as_before_under_a_lock() {
    let empty: [&str; 0] = [];
    for body in [
        "locked.count += apply(n => n + 1)",
        "numbers = [1]\nlocked.count += numbers.len()",
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

/// A source's `else:` that fails with a case written with a leading dot, `.Stuck(...)`, is
/// written in where it's used with its enum named, as a note on the block's failure, where no
/// expected type would say which enum it's from.
#[test]
fn a_failing_else_with_a_leading_dot_case_is_written_in() {
    let source = "error Shut:\n    Stuck(name: str)\n    Jammed\n\ndef opened(name: str) -> with str fails Shut:\n    lend name else:\n        fail .Stuck(name=name)\n\ndef single() fails Shut:\n    with opened(\"single\")? as door:\n        fail .Jammed\n\ndef main(stdout: Stdout):\n    single() else as why:\n        stdout.print(\"failed\", why, why.notes())\n        return\n    stdout.print(\"worked\")\n";
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
             def main(stdout: Stdout, clock: Clock, jobs: Channel[int]) fails Closed | Timeout:\n    \
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
            "{declared}def main(stdout: Stdout, clock: Clock, sender: Sender[int]) fails Closed | Timeout:\n    shared scores = Scores()\n{body}"
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
