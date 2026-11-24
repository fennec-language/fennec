//! What a `with` lends stays in its block, whose end finishes it: it can't be given back, yielded,
//! kept in a place made outside the block, sent on a channel, or read by an arrow function kept
//! outside it. A second name for it in the block is fine, and so are reading it, handing it to a
//! function and building values from what's read from it.

use crate::problems;

/// A source the program writes, lending a `Tx` to its caller's block, and a struct holding one.
const STORE: &str = "
from std.concurrent import Closed, Sender

struct Tx:
    log: str

struct Store:
    name: str

    def transaction(self) -> with Tx:
        lend Tx(log='tx') except:
            pass

struct Kept:
    tx: Tx

def keep(ref into: list[Tx], tx: Tx):
    into.append(tx)

def count(ref total: int, tx: Tx):
    total += len(tx.log)

def show(tx: Tx) -> int:
    return len(tx.log)
";

/// The problems in `body`, written after `STORE`.
fn with_store(body: &str) -> Vec<String> {
    problems(&format!("{STORE}\n{body}"))
}

/// Where its block is.
const OUTSIDE: &str = "is made outside the block of the `with` that lends it";

#[test]
fn what_a_with_lends_cant_be_given_back_or_yielded() {
    let gives = "this gives back `tx`, which its `with` lends only for its block";
    let returned = "def kept(store: Store) -> Tx:\n    with store.transaction() as tx:\n        return tx\n    return Tx(log='none')\n";
    assert_eq!(with_store(returned), [format!("{gives} @ tx")]);
    let in_a_list = "def kept(store: Store) -> list[Tx]:\n    with store.transaction() as tx:\n        return [Tx(log='new'), tx]\n    return []\n";
    assert_eq!(with_store(in_a_list), [format!("{gives} @ [Tx(log=\"new\"), tx]")]);
    let in_a_struct = "def kept(store: Store) -> Kept:\n    with store.transaction() as tx:\n        return Kept(tx=tx)\n    return Kept(tx=Tx(log='none'))\n";
    assert_eq!(with_store(in_a_struct), [format!("{gives} @ Kept(tx=tx)")]);
    let yielded =
        "def each(store: Store) -> Iterator[Tx]:\n    with store.transaction() as tx:\n        yield tx\n";
    assert_eq!(with_store(yielded), ["this yields `tx`, which its `with` lends only for its block @ tx"]);
}

#[test]
fn what_a_with_lends_cant_be_kept_outside_its_block() {
    let named = "def kept(store: Store) -> int:\n    mut outer = Tx(log='none')\n    with store.transaction() as tx:\n        outer = tx\n    return len(outer.log)\n";
    assert_eq!(with_store(named), [format!("this keeps `tx` in `outer`, which {OUTSIDE} @ tx")]);
    let appended = "def kept(store: Store) -> int:\n    mut outer = list[Tx]()\n    with store.transaction() as tx:\n        outer.append(tx)\n    return len(outer)\n";
    assert_eq!(with_store(appended), [format!("this puts `tx` in `outer`, which {OUTSIDE} @ tx")]);
    let in_a_dict = "def kept(store: Store) -> int:\n    mut outer = dict[str, Tx]()\n    with store.transaction() as tx:\n        outer['a'] = tx\n    return len(outer)\n";
    assert_eq!(with_store(in_a_dict), [format!("this keeps `tx` in `outer`, which {OUTSIDE} @ tx")]);
    let in_a_field = "def kept(store: Store) -> int:\n    mut holder = Kept(tx=Tx(log='none'))\n    with store.transaction() as tx:\n        holder.tx = tx\n    return len(holder.tx.log)\n";
    assert_eq!(with_store(in_a_field), [format!("this keeps `tx` in `holder`, which {OUTSIDE} @ tx")]);
    // Through a second name, and a list made inside the block.
    let again = "def kept(store: Store) -> int:\n    mut outer = list[Tx]()\n    with store.transaction() as tx:\n        again = tx\n        mut inside = [again]\n        outer.extend(inside)\n    return len(outer)\n";
    assert_eq!(with_store(again), [format!("this puts `inside` in `outer`, which {OUTSIDE} @ inside")]);
    let looped = "def kept(store: Store) -> int:\n    mut outer = list[Tx]()\n    with store.transaction() as tx:\n        held = [tx, Tx(log='new')]\n        for each in held:\n            outer.append(each)\n    return len(outer)\n";
    assert_eq!(with_store(looped), [format!("this puts `each` in `outer`, which {OUTSIDE} @ each")]);
    let by_ref = "def kept(store: Store) -> int:\n    mut outer = list[Tx]()\n    with store.transaction() as tx:\n        keep(ref outer, tx)\n    return len(outer)\n";
    assert_eq!(with_store(by_ref), [format!("this puts `tx` in `outer`, which {OUTSIDE} @ tx")]);
    let arrow = "def kept(store: Store) -> int:\n    mut later: () -> int = () => 0\n    with store.transaction() as tx:\n        later = () => len(tx.log)\n    return later()\n";
    assert_eq!(
        with_store(arrow),
        [format!("this keeps `tx` in `later`, which {OUTSIDE} @ () => len(tx.log)")]
    );
    let sent = "def sent(store: Store, out: Sender[Tx]) fails Closed:\n    with store.transaction() as tx:\n        out.send(tx)?\n";
    assert_eq!(with_store(sent), [format!("this puts `tx` in `out`, which {OUTSIDE} @ tx")]);
}

#[test]
fn what_a_with_lends_is_used_freely_inside_its_block() {
    let fine = "def used(store: Store) -> int:\n    with store.transaction() as tx:\n        again = tx\n        mut inside = [tx, again]\n        inside.append(tx)\n        mut total = 0\n        count(ref total, tx)\n        made = Tx(log=tx.log)\n        return show(again) + len(inside) + total + len(made.log) + len(tx.log)\n    return 0\n";
    assert_eq!(with_store(fine), Vec::<String>::new());
    // What's read from it, and a value built from that, can leave.
    let read = "def read(store: Store) -> Tx:\n    with store.transaction() as tx:\n        return Tx(log=tx.log + '!')\n    return Tx(log='none')\n";
    assert_eq!(with_store(read), Vec::<String>::new());
    // A list made in the block can hold what it lends, but not what a `with` inside it lends.
    let nested = "def nested(store: Store) -> int:\n    with store.transaction() as tx:\n        mut seen = list[Tx]()\n        with store.transaction() as inner:\n            seen.append(tx)\n            seen.append(inner)\n        return len(seen)\n    return 0\n";
    assert_eq!(with_store(nested), [format!("this puts `inner` in `seen`, which {OUTSIDE} @ inner")]);
}

/// #2160's program: a file `fs.create` lends, given back and kept in a list made outside its block.
/// Both were let through, and a write through the file kept failed at run time.
#[test]
fn a_file_cant_be_kept_past_its_block() {
    let program = "
        from std.fs import File

        def kept(fs: Fs) -> File fails IoError:
            with fs.create('first.txt')? as file:
                file.write_line('in the block')?
                return file
            fail .Other(reason='not reached')

        def main(stdout: Stdout, fs: Fs) fails IoError:
            mut files = list[File]()
            with fs.create('second.txt')? as file:
                files.append(file)
            stdout.print(len(files))
            return
    ";
    assert_eq!(
        problems(program),
        [
            "this gives back `file`, which its `with` lends only for its block @ file",
            format!("this puts `file` in `files`, which {OUTSIDE} @ file").as_str(),
        ]
    );
}

/// A number, text or bytes a source lends is a copy, with nothing for the block's end to finish,
/// so it leaves as any value does.
#[test]
fn a_plain_value_a_with_lends_is_a_copy() {
    let program = "
        def counted(items: list[int]) -> with int:
            lend len(items)

        def named() -> with str:
            lend 'ana'

        def count(items: list[int]) -> list[int]:
            mut kept = list[int]()
            with counted(items) as total:
                kept.append(total)
            with named() as name:
                kept.append(len(name))
            with counted(items) as total:
                return [total] + kept
            return kept
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

/// A channel's end, as the mailbox `with launch(...)` lends a task, is made to be handed to other
/// tasks: a hub may keep it past the block, and a send to it after that fails with `Closed`, which
/// the hub deals with. So it can leave its block, as `bench/server/chat.fen` hands each
/// connection's mailbox to its hub.
#[test]
fn a_channels_end_can_be_handed_on() {
    let program = "
        from std.concurrent import Closed, Crashed, Inbox, Sender

        enum Note:
            Joined(name: str, mailbox: Sender[str])

        def deliver(inbox: Inbox[str]) -> int:
            mut count = 0
            for _ in inbox:
                count += 1
            return count

        def attend(name: str, to_hub: Sender[Note]) fails Closed | Crashed:
            with launch((inbox) => deliver(inbox), inbox_size=4)? as mailbox:
                to_hub.send(.Joined(name=name, mailbox=mailbox))?
            return
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

/// An interface's `with` source runs from a generic function, which is copied for each type it's
/// called with. A method, of a generic type or with type parameters of its own, has no such copy,
/// so calling one there is refused where it's written, saying what to write instead.
#[test]
fn an_interfaces_source_runs_from_a_generic_function_not_a_method() {
    let types = "error Nope:\n    why: str\n\nstruct Tx:\n    log: str\n\n\
                 interface Database:\n    def transaction(self) -> with Tx fails Nope\n\n";
    let source = |rest: &str| problems(&format!("{types}{rest}"));
    let in_method = "struct Repo[D: Database]:\n    db: D\n\n    def saved(self) -> str fails Nope:\n        \
                     with self.db.transaction()? as tx:\n            return tx.log\n        return \"\"\n";
    assert_eq!(
        source(in_method),
        [
            "`transaction` is a `with` source of `D`'s interface, which runs from a generic function, not from a method like `Repo.saved` @ self.db.transaction()"
        ]
    );
    let own_parameter = "struct Repo:\n    name: str\n\n    def saved[D: Database](self, db: D) -> str fails Nope:\n        \
                         with db.transaction()? as tx:\n            return tx.log\n        return \"\"\n";
    assert_eq!(source(own_parameter).len(), 1, "{:?}", source(own_parameter));
    let in_function = "struct Repo[D: Database]:\n    db: D\n\n\
                       def saved[D: Database](repo: Repo[D]) -> str fails Nope:\n    \
                       with repo.db.transaction()? as tx:\n        return tx.log\n    return \"\"\n";
    let empty: [&str; 0] = [];
    assert_eq!(source(in_function), empty);
}
