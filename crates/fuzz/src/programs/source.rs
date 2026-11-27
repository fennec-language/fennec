//! A program tree written out as Fennec.

#![expect(clippy::too_many_lines, reason = "one match writes each statement, and one each expression")]

use std::fmt::Write as _;

use super::ast::{
    Alias, Aliasing, Amount, Arith, Arrow, BufOp, Case, Compare, Constants, Expr, FLOATS, FloatHeld, Held,
    KEYS, Kept, LEDGER, Leave, Nested, Op, Place, Program, ROOMS, Reading, RefPlace, STARTING, Stmt,
    StreamMap, StreamRead, Streaming, TABLE, Turn, Turning, Via, Writing, XS, YS, alias_name, starting,
};
use super::ast::{ArrayStep, ArrayWay, Arraying};
use super::ast::{
    Between, Calling, CaptureWay, Capturing, Field, FieldCall, FieldCalls, Listening, Looping, REF_TEXTS,
    RefCasing, RefWay, SWAP_KEYS, ShelfPlace, Shelve, Shelving, SwapField, Swapping, Waking, Yielding,
};
use super::ast::{FILE_PIECES, FileWay, Filing};
use super::ast::{GenStep, GenWay, Generating};
use super::ast::{GenericStep, GenericTy, Genericking};
use super::ast::{IfaceStep, Interfacing};
use super::ast::{InOrder, OrderStep};
use super::ast::{LockWay, LockingTogether};
use super::ast::{NarrowWay, Narrowing};
use super::ast::{OwnIndexing, OwnStep};
use super::ast::{PopTy, PopWay, Popping};
use super::ast::{RuleStep, RuleWay, Ruling};
use super::ast::{SizedStep, SizedWay, Sizing};
use super::ast::{Sticking, StuckWay};

/// What every program starts with, for its body to use.
const HEADER: &str = "from std.collections import Table
from std.concurrent import Closed, Crashed, Inbox, Timeout

struct Point:
    x: int
    y: int

    def total(self) -> int:
        return self.x + self.y

    def moved(ref self, by: int):
        self.x += by

enum Shape:
    Circle(radius: int)
    Square(side: int)
    Empty

def area(shape: Shape) -> int:
    match shape:
        case .Circle(radius): return radius * radius * 3
        case .Square(side): return side * side
        case .Empty: return 0

def at_or_zero(items: list[int], at: int) -> int:
    if len(items) == 0: return 0
    return items[at % len(items)]

def bump(ref value: int, by: int) -> int:
    value += by
    return value * 10

error Refused:
    code: int

def change(box: shared Point, by: int, refuse: bool) fails Refused:
    lock box as ref held:
        held.x += by
        held.y = held.y * 2
        if refuse:
            fail Refused(code=by)

def grow_all(ref items: list[int], by: int):
    for ref item in items:
        item += by

def appended_all(ref items: list[int], more: list[int]):
    for item in more:
        items.append(item)

def buffer_of(made: Buffer[int], items: list[int]) -> Buffer[int]:
    mut buffer = made
    for item in items:
        buffer.push(item)
    return buffer

def counted(limit: int) -> Iterator[int]:
    for at in range(limit):
        yield at * 3

struct Tally:
    count: int

struct Bag:
    items: list[int]
    counts: dict[str, int]

    def add_twice(ref self, n: int):
        self.items.append(n)
        self.items.append(n)

struct Outer:
    inner: shared Bag
    n: int

def bump_outer(outer: shared Outer, by: int, refuse: bool) fails Refused:
    lock outer as ref held:
        held.n += by
        if refuse:
            fail Refused(code=by)

error Shut:
    code: int

def shutting(code: int, stdout: Stdout) -> with Tally fails Shut:
    stdout.print(\"opening\", code)
    lend Tally(count=code) except:
        stdout.print(\"undoing\", code)
    stdout.print(\"closing\", code)
    if code % 2 == 0:
        fail Shut(code=code)

def lent_counts(limit: int, stdout: Stdout) -> Iterator[int] fails Shut:
    with shutting(limit, stdout)? as tally:
        for at in range(limit):
            yield at + tally.count

def summed_until(limit: int, stop: int, stdout: Stdout) -> int fails Shut:
    mut total = 0
    for value in lent_counts(limit, stdout)?:
        if value == stop:
            break
        total += value
    return total

def summed_shown(limit: int, stop: int, stdout: Stdout) -> str:
    total = summed_until(limit, stop=stop, stdout=stdout) except why:
        case Shut():
            return \"shut\"
    return str(total)

def failing_counts(limit: int, at: int) -> Iterator[int] fails Shut:
    for each in range(limit):
        if each == at:
            fail Shut(code=each)
        yield each

def enumerated_until(limit: int, stop: int, stdout: Stdout) -> int fails Shut:
    mut total = 0
    for place, value in enumerate(lent_counts(limit, stdout)?):
        if value == stop:
            break
        total += value + place
    return total

def zipped_until(limit: int, stop: int, words: list[int], stdout: Stdout) -> int fails Shut:
    mut total = 0
    for value, word in zip(lent_counts(limit, stdout)?, words):
        if value == stop:
            break
        total += value + word
    return total

def zipped_after(limit: int, stop: int, words: list[int], stdout: Stdout) -> int fails Shut:
    mut total = 0
    for word, value in zip(words, lent_counts(limit, stdout)?):
        if value == stop:
            break
        total += value + word
    return total

def enumerated_failing(limit: int, at: int) -> int fails Shut:
    mut total = 0
    for place, value in enumerate(failing_counts(limit, at=at)?):
        total += value + place
    return total

def zipped_failing(limit: int, at: int, words: list[int]) -> int fails Shut:
    mut total = 0
    for value, word in zip(failing_counts(limit, at=at)?, words):
        total += value + word
    return total

def zipped_failing_after(limit: int, at: int, words: list[int]) -> int fails Shut:
    mut total = 0
    for word, value in zip(words, failing_counts(limit, at=at)?):
        total += value + word
    return total

def through_shown(way: int, limit: int, stop: int, words: list[int], stdout: Stdout) -> str:
    if way == 0:
        got = enumerated_until(limit, stop=stop, stdout=stdout) except why:
            case Shut():
                return \"shut\"
        return str(got)
    if way == 1:
        got = zipped_until(limit, stop=stop, words=words, stdout=stdout) except why:
            case Shut():
                return \"shut\"
        return str(got)
    if way == 2:
        got = zipped_after(limit, stop=stop, words=words, stdout=stdout) except why:
            case Shut():
                return \"shut\"
        return str(got)
    if way == 3:
        got = enumerated_failing(limit, at=stop) except why:
            case Shut():
                return \"shut\"
        return str(got)
    if way == 4:
        got = zipped_failing(limit, at=stop, words=words) except why:
            case Shut():
                return \"shut\"
        return str(got)
    got = zipped_failing_after(limit, at=stop, words=words) except why:
        case Shut():
            return \"shut\"
    return str(got)

struct Holder:
    items: list[int]
    tag: int

    def grown_by(ref self, more: list[int]):
        for item in more:
            self.items.append(item)

struct Unit:
    tags: list[int]
    hp: int

struct Army:
    units: dict[str, Unit]

struct Session:
    log: str

struct Lite:
    name: str

    def run(self, text: str) -> int:
        return len(text)

    def session(self) -> with Session:
        lend Session(log=\"lite\") except:
            pass

struct Pg:
    host: str

    def run(self, text: str) -> int:
        return len(text) * 2

    def session(self) -> with Session:
        lend Session(log=\"pg\") except:
            pass

struct Mem:
    size: int

    def run(self, text: str) -> int:
        return len(text) * 3

    def session(self) -> with Session:
        lend Session(log=\"mem\") except:
            pass

type Two = Lite | Pg
type Three = Lite | Pg | Mem

def opened(store: Lite, text: str) -> str:
    with store.session() as session:
        return f\"{session.log} {store.run(text)}\"

def matched2(store: Two, text: str) -> str:
    match store:
        case Lite(name=_):
            with store.session() as session:
                return f\"{session.log} {store.run(text)}\"
        case Pg(host=_):
            with store.session() as session:
                return f\"{session.log} {store.run(text)}\"

def checked2(store: Two, text: str) -> str:
    if store is Lite:
        return opened(store, text)
    if store is Pg:
        with store.session() as session:
            return f\"{session.log} {store.run(text)}\"
    return \"\"

def direct2(store: Two, text: str) -> str:
    count = store.run(text)
    if store is Lite:
        with store.session() as session:
            return f\"{session.log} {count}\"
    if store is Pg:
        with store.session() as session:
            return f\"{session.log} {count}\"
    return \"\"

def matched3(store: Three, text: str) -> str:
    match store:
        case Lite(name=_):
            with store.session() as session:
                return f\"{session.log} {store.run(text)}\"
        case Pg(host=_):
            with store.session() as session:
                return f\"{session.log} {store.run(text)}\"
        case Mem(size=_):
            with store.session() as session:
                return f\"{session.log} {store.run(text)}\"

def checked3(store: Three, text: str) -> str:
    if store is Lite:
        return opened(store, text)
    if store is Pg:
        with store.session() as session:
            return f\"{session.log} {store.run(text)}\"
    if store is Mem:
        with store.session() as session:
            return f\"{session.log} {store.run(text)}\"
    return \"\"

def plain2(store: Two, text: str) -> str:
    with store.session() as session:
        return f\"{session.log} {store.run(text)}\"

def plain3(store: Three, text: str) -> str:
    with store.session() as session:
        return f\"{session.log} {store.run(text)}\"

def rested(rest: Pg | Mem, text: str) -> int:
    return rest.run(text)

def rest3(store: Three, text: str) -> str:
    if store is Lite:
        return opened(store, text)
    count = store.run(text)
    given = rested(store, text)
    match store:
        case Pg(host=_):
            with store.session() as session:
                return f\"{session.log} {count}\"
        case Mem(size=_):
            with store.session() as session:
                return f\"{session.log} {given}\"

def direct3(store: Three, text: str) -> str:
    count = store.run(text)
    if store is Lite:
        with store.session() as session:
            return f\"{session.log} {count}\"
    if store is Pg:
        with store.session() as session:
            return f\"{session.log} {count}\"
    if store is Mem:
        with store.session() as session:
            return f\"{session.log} {count}\"
    return \"\"

def capped(n: int, limit: int) -> int fails Refused:
    if n > limit:
        fail Refused(code=n)
    return n

def shut_below(n: int, limit: int) -> int fails Shut:
    if n < -limit:
        fail Shut(code=n)
    return n

def tried(run: () -> int fails Refused) -> str:
    value = run() except why:
        return f\"refused {why.code}\"
    return str(value)

def fallback(run: () -> int fails Refused) -> str:
    value = run() except why:
        why.code * 100
    return str(value)

def tried_twice(run: () -> int fails Refused) -> str fails Refused:
    first = run()?
    second = run()?
    return f\"{first} {second}\"

## Whether `run` gives its value rather than failing.
def ran_fine(run: () -> int fails Refused) -> bool:
    _ = run() except why:
        case Refused():
            return False
    return True

def tried_each(items: list[int], run: (int) -> int fails Refused) -> str:
    mut kept = list[int]()
    for item in items:
        value = run(item) except why:
            return f\"refused {why.code} after {kept}\"
        kept.append(value)
    return str(kept)

def paired(a: int, b: int) -> str:
    return f\"{a} {b}\"

def either(run: () -> str fails Refused | Shut) -> str:
    text = run() except why:
        case Refused(code=code):
            return f\"refused {code}\"
        case Shut(code=code):
            return f\"shut {code}\"
    return text

struct Gauge:
    at: float
    n: int

struct F32Gauge:
    at: f32

enum Level:
    At(value: float)
    Off

enum Level32:
    At(value: f32)
    Off

type Marked = Gauge | str

def nan() -> float:
    big = 1e308 * 10.0
    return big - big

struct Spot:
    label: str
    x: int

enum Change:
    Move(at: int, by: int)
    Rename(at: int, label: str)

def plan(x: int, world: list[Spot]) -> Change:
    at = x % len(world)
    if x % 2 == 0:
        return .Move(at=at, by=x)
    return .Rename(at=at, label=f\"<{x}>\")

def regrown(seed: int) -> int:
    mut holder = Holder(items=[seed, seed + 1], tag=seed)
    holder.items.append(holder.tag)
    own = holder.items
    holder.grown_by(own)
    return sum(holder.items) + len(own)

enum Node:
    Leaf(name: str)
    Number(value: int)
    Branch(label: str, children: list[Node])
    Tagged(tags: list[str], inner: list[Node])

def deep(depth: int, label: str) -> Node:
    mut node = Node.Leaf(name=label)
    for _ in range(depth):
        node = .Branch(label=label, children=[node])
    return node

def depth_of(node: Node) -> int:
    mut at = node
    mut depth = 0
    while True:
        match at:
            case .Branch(_, children):
                if len(children) == 0:
                    return depth
                at = children[0]
                depth += 1
            case _:
                return depth
    return depth

def digest(node: Node) -> str:
    match node:
        case .Leaf(name):
            return \"L(\" + name + \")\"
        case .Number(value):
            return f\"N({value})\"
        case .Branch(label, children):
            mut text = \"B(\" + label
            for child in children:
                text += \" \" + digest(child)
            return text + \")\"
        case .Tagged(tags, inner):
            mut text = \"T(\"
            for tag in tags:
                text += tag + \",\"
            for child in inner:
                text += \" \" + digest(child)
            return text + \")\"

def digest_all(nodes: list[Node]) -> str:
    mut text = \"[\"
    for node in nodes:
        text += digest(node) + \";\"
    return text + \"]\"

def node_or_leaf(nodes: list[Node], at: int) -> Node:
    if len(nodes) == 0: return .Leaf(name=\"none\")
    return nodes[at % len(nodes)]

enum Named:
    Leaf(name: str)
    Pair(left: Named, right: Named, label: str, tags: list[str])

def chain(depth: int, label: str) -> Named:
    mut named = Named.Leaf(name=label)
    for at in range(depth):
        named = .Pair(left=named, right=.Leaf(name=f\"<{at}>\"), label=label, tags=[label])
    return named

def named_digest(named: Named) -> str:
    match named:
        case .Leaf(name):
            return \"l(\" + name + \")\"
        case .Pair(left, right, label, tags):
            mut text = \"p(\" + label
            for tag in tags:
                text += \",\" + tag
            return text + \" \" + named_digest(left) + \" \" + named_digest(right) + \")\"

def named_depth(named: Named) -> int:
    mut at = named
    mut depth = 0
    while True:
        match at:
            case .Pair(left, _, _, _):
                at = left
                depth += 1
            case _:
                return depth
    return depth

enum Held:
    Leaf(name: str)
    Pair(left: Held, right: Held, label: str, tags: list[str])

def held_digest(held: Held) -> str:
    match held:
        case .Leaf(name):
            return \"h(\" + name + \")\"
        case .Pair(left, right, label, tags):
            mut text = \"q(\" + label
            for tag in tags:
                text += \",\" + tag
            return text + \" \" + held_digest(left) + \" \" + held_digest(right) + \")\"

def ward(name: str, stdout: Stdout) -> with str:
    stdout.print(\"open\", name)
    lend name except:
        stdout.print(\"undo\", name)
    stdout.print(\"close\", name)

def present(i: int, at: int) -> int?:
    if i == at:
        return None
    return i

def guarded(code: int, stdout: Stdout) -> with Tally fails Refused:
    stdout.print(\"guard\", code)
    lend Tally(count=code) except:
        stdout.print(\"unguard\", code)
        if code % 3 == 0:
            fail Refused(code=code + 1000)
        stdout.print(\"unguarded\", code)
    stdout.print(\"released\", code)

def guarded_noted(code: int, stdout: Stdout) -> with Tally fails Refused:
    stdout.print(\"guard\", code)
    lend Tally(count=code) except:
        stdout.print(\"unguard\", code)
        if code % 3 == 0:
            fail Refused(code=code + 1000)
        kept = capped(code, limit=20)?
        stdout.print(\"unguarded\", kept)
    stdout.print(\"released\", code)

def guarded_run(code: int, refuse: bool, crash: bool, noted: bool, stdout: Stdout) -> int fails Refused:
    if noted:
        with guarded_noted(code, stdout)? as tally:
            stdout.print(\"inside\", tally.count)
            if refuse:
                fail Refused(code=tally.count)
            if crash:
                _ = [1, 2][tally.count]
            return tally.count
    with guarded(code, stdout)? as tally:
        stdout.print(\"inside\", tally.count)
        if refuse:
            fail Refused(code=tally.count)
        if crash:
            _ = [1, 2][tally.count]
        return tally.count

def guarded_shown(code: int, refuse: bool, crash: bool, noted: bool, stdout: Stdout) -> str:
    value = guarded_run(code, refuse=refuse, crash=crash, noted=noted, stdout=stdout) except why:
        return f\"refused {why.code} {why.notes()}\"
    return str(value)

def tallied(start: int, stdout: Stdout) -> with Tally:
    stdout.print(\"open\", start)
    lend Tally(count=start) except:
        stdout.print(\"undone\", start)
    stdout.print(\"closed\", start)

";

/// The program as Fennec source.
/// The functions that launch a task, left out of a program that has no other threads.
const TASKS: &str = "def tallying(inbox: Inbox[int], totals: shared Point):
    for number in inbox:
        lock totals as ref held:
            held.y += 1
            held.x += number

def tally(totals: shared Point, items: list[int]) fails Crashed | Closed | Timeout:
    with launch((inbox) => tallying(inbox, totals))? as sends:
        for item in items:
            sends.send(item)?

def guarding(inbox: Inbox[int], stdout: Stdout) fails Refused:
    for number in inbox:
        with guarded(number, stdout)? as tally:
            stdout.print(\"task\", tally.count)
            if number == 5:
                _ = [1, 2][tally.count]
            if number == 7:
                fail Refused(code=number)

def guard_all(items: list[int], stdout: Stdout) fails Crashed | Closed | Timeout | Refused:
    with launch((inbox) => guarding(inbox, stdout)?)? as sends:
        for item in items:
            sends.send(item)?

";

pub(crate) fn source(program: &Program) -> String {
    let mut writer = Writer { out: String::new(), helpers: String::new(), made: 0 };
    writer.block(&program.body, 1);
    writer.line(1, "stdout.print(\"done\")");
    let tasks = if program.threadless { "" } else { TASKS };
    // Only a `Yielded` statement's task sends back over a channel, and only a `Listened` statement
    // takes a channel whole, so only a program holding one imports what that takes, and every
    // other program is written as it always was.
    let channels = match (writer.helpers.contains("Channel["), writer.helpers.contains("Sender[")) {
        (true, _) => "from std.concurrent import Channel, Sender, channel\n",
        (false, true) => "from std.concurrent import Sender, channel\n",
        (false, false) => "",
    };
    // Only a program holding a `Filed` statement reaches the disk, so only it takes an `Fs`.
    let fs = if writer.helpers.contains("(fs: Fs, stdout: Stdout)") { ", fs: Fs" } else { "" };
    // Only a program holding a `Saved` statement saves values, so only it imports the formats and
    // names `Id`.
    let (formats, header) = if writer.helpers.contains("json.loads[") {
        ("from std import json, pack, toml\n", HEADER.replacen("import Table", "import Id, Table", 1))
    } else {
        ("", HEADER.to_owned())
    };
    // Only a program holding a `Stored` statement opens a database, so only it imports them.
    let databases = if writer.helpers.contains("sqlite.open(") {
        "from std import sqlite\nfrom std.sql import SqlError\n"
    } else {
        ""
    };
    // Only a program holding a `Pooled` statement makes a pool, so only it imports one.
    let pools = if writer.helpers.contains("with pool[") {
        "from std.concurrent import Pool, PoolError, pool\n"
    } else {
        ""
    };
    format!(
        "{formats}{databases}{pools}{channels}{header}{tasks}{}def main(stdout: Stdout{fs}):\n{}",
        writer.helpers, writer.out
    )
}

/// `streamed{id}`, the function a `Streamed` calls: its stream, made and read.
fn streamed_written(streaming: &Streaming) -> String {
    let Streaming { id, items, map, read } = streaming;
    let items: Vec<String> = items.iter().map(ToString::to_string).collect();
    let mut out = format!("def streamed{id}(stdout: Stdout):\n    xs = [{}]\n", items.join(", "));
    let each = match map {
        StreamMap::Scaled(by, plus) => format!("item * {by} + {plus}"),
        StreamMap::Divided(zero) => format!("100 // (item - {zero})"),
        StreamMap::Indexed => {
            let ys: Vec<String> = YS.iter().map(ToString::to_string).collect();
            let _ = writeln!(out, "    ys = [{}]", ys.join(", "));
            "ys[item]".to_owned()
        }
    };
    let _ = writeln!(out, "    stream = ({each} for item in parallel(xs))");
    match read {
        StreamRead::Loop(stop) => {
            out.push_str("    mut seen = 0\n    for y in stream:\n        stdout.print(\"streamed\", y)\n        seen += 1\n");
            if let Some(stop) = stop {
                let _ = writeln!(out, "        if seen == {stop}:\n            break");
            }
            out.push_str("    stdout.print(\"read\", seen)\n");
        }
        StreamRead::Sum => out.push_str("    stdout.print(\"sum\", sum(y for y in stream))\n"),
        StreamRead::Listed => out.push_str("    stdout.print([y + 1 for y in stream])\n"),
    }
    out.push('\n');
    out
}

/// `yielded{id}`, the function a `Yielded` calls, after its types, generators and task.
fn yielded_written(yielding: &Yielding) -> String {
    let Yielding { id, count, widths: [notes, pairs, words] } = *yielding;
    format!(
        "struct Note{id}:\n    text: str\n    items: list[int]\n    counts: dict[str, int]\n\n\
         enum Word{id}:\n    Plain{id}(text: str)\n    Many{id}(items: list[int])\n\n\
         def notes{id}(count: int, width: int) -> Iterator[Note{id}]:\n    \
         mut kept = Note{id}(text=\"k\" * width, items=[0], counts={{\"k\": 0}})\n    for at in range(count):\n        \
         yield Note{id}(text=str(at) + \"n\" * width, items=[at, at + 1], counts={{\"a\": at}})\n        \
         kept.items.append(at)\n        kept.counts[\"k\"] += at\n        yield kept\n\n\
         def pairs{id}(count: int, width: int) -> Iterator[(str, list[int])]:\n    for at in range(count):\n        \
         yield (str(at) + \"p\" * width, [at])\n\n\
         def words{id}(count: int, width: int) -> Iterator[Word{id}]:\n    for at in range(count):\n        \
         if at % 2 == 0:\n            yield .Plain{id}(text=str(at) + \"w\" * width)\n        else:\n            \
         yield .Many{id}(items=[at, at])\n\n\
         def echoed{id}(inbox: Inbox[Note{id}], back: Sender[Note{id}]) fails Closed:\n    for note in inbox:\n        \
         back.send(note)?\n    return\n\n\
         def yielded{id}(stdout: Stdout) fails Crashed | Closed | Timeout:\n    mut got = list[Note{id}]()\n    \
         for note in notes{id}({count}, width={notes}):\n        got.append(note)\n    for note in got:\n        \
         stdout.print(\"note\", note)\n    mut firsts = list[(str, list[int])]()\n    \
         for pair in pairs{id}({count}, width={pairs}):\n        firsts.append(pair)\n    stdout.print(\"pairs\", firsts)\n    \
         for word in words{id}({count}, width={words}):\n        match word:\n            case .Plain{id}(text):\n                \
         stdout.print(\"plain\", text)\n            case .Many{id}(items):\n                stdout.print(\"many\", items)\n    \
         with channel[Note{id}](size=64) as replies:\n        back = replies.sender()\n        \
         with launch((inbox) => echoed{id}(inbox, back)?)? as sends:\n            for note in got:\n                \
         sends.send(note)?\n        for _ in range(len(got)):\n            note = replies.receive() else break\n            \
         stdout.print(\"back\", note)\n    return\n\n"
    )
}

/// `woken{id}`, the function a `Woken` calls, after the task it launches.
fn woken_written(waking: &Waking) -> String {
    let Waking { id, count, at, failing, .. } = *waking;
    let leaving = if failing { "fail Refused(code=number)" } else { "_ = [1][number - at + 1]" };
    format!(
        "def answering{id}(inbox: Inbox[int], back: Sender[int], at: int) fails Closed | Refused:\n    \
         for number in inbox:\n        if number == at:\n            {leaving}\n        back.send(number * 2)?\n    \
         return\n\n\
         def woken{id}(stdout: Stdout) fails Crashed | Closed | Timeout | Refused:\n    \
         with channel[int](size=1) as replies:\n        back = replies.sender()\n        \
         with launch((inbox) => answering{id}(inbox, back, at={at})?)? as sends:\n            \
         for number in range({count}):\n                sends.send(number)?\n                \
         got = replies.receive() else break\n                stdout.print(\"got\", got)\n    return\n\n"
    )
}

/// `swaps{id}`, the function a `Swapped` calls, after the struct and the functions it uses.
fn swapped_written(swapping: &Swapping) -> String {
    let Swapping { id, base, other_base, field, key, fresh, called, kept, owned, rounds } = *swapping;
    let lists = field == SwapField::Lists;
    let field = if lists { "lists" } else { "counts" };
    let key = SWAP_KEYS.get(key).copied().unwrap_or("a");
    let other = if called {
        format!("desk{id}({other_base})")
    } else {
        format!("Desk{id}(counts={{\"c\": {other_base}}}, lists={{\"c\": [{other_base}]}})")
    };
    let stored = if lists { "[fresh]" } else { "fresh" };
    let after_store = if kept {
        format!("    if known is not None:\n        mine.{field}[\"known\"] = known\n")
    } else if lists {
        format!("    mine.{field}[\"found\"] = [1] if known is not None else [0]\n")
    } else {
        format!("    mine.{field}[\"found\"] = 1 if known is not None else 0\n")
    };
    let call = if owned {
        format!(
            "        given = desk{id}({base} + turn)\n        after = swapped{id}(given, key=\"{key}\", fresh={fresh})\n        \
             stdout.print(given, after)\n"
        )
    } else {
        format!(
            "        after = swapped{id}(desk{id}({base} + turn), key=\"{key}\", fresh={fresh})\n        stdout.print(after)\n"
        )
    };
    format!(
        "struct Desk{id}:\n    counts: dict[str, int]\n    lists: dict[str, list[int]]\n\n\
         def desk{id}(base: int) -> Desk{id}:\n    \
         return Desk{id}(counts={{\"a\": base, \"b\": base + 1}}, lists={{\"a\": [base, 1], \"b\": [base + 2]}})\n\n\
         def swapped{id}(given: Desk{id}, key: str, fresh: int) -> Desk{id}:\n    mut mine = given\n    \
         other = {other}\n    known = mine.{field}.get(key)\n    mine = other\n    mine.{field}[key] = {stored}\n\
         {after_store}    return mine\n\n\
         def swaps{id}(stdout: Stdout):\n    for turn in range({rounds}):\n{call}    return\n\n"
    )
}

/// `shelved{id}`, the function a `Shelved` calls, after the types and the functions it uses.
fn shelved_written(shelving: &Shelving) -> String {
    let Shelving { id, in_list, seen, place, rounds, ref steps } = *shelving;
    let (held, at, made) = if in_list {
        (
            format!("list[Pouch{id}]"),
            "0",
            format!("[Pouch{id}(items=[\"first, {LONG_PIECE}\"], seen={seen})]"),
        )
    } else {
        (
            format!("dict[str, Pouch{id}]"),
            "\"one\"",
            format!("{{\"one\": Pouch{id}(items=[\"first, {LONG_PIECE}\"], seen={seen})}}"),
        )
    };
    let steps: Vec<String> = steps
        .iter()
        .map(|(value, how)| {
            let how = match how {
                Shelve::Fill => 0,
                Shelve::ByHand => 1,
                Shelve::Direct => 2,
                Shelve::Handed => 3,
                Shelve::HandedByHand => 4,
            };
            format!("({value}, {how})")
        })
        .collect();
    let steps = steps.join(", ");
    let types = format!(
        "struct Pouch{id}:\n    items: list[str]\n    seen: int\n\n    \
         def add(ref self, value: int, limits: list[int]):\n        for turn in range({rounds}):\n            \
         self.items.append(f\"added {{value}} {{turn}}, {LONG_PIECE}\")\n            \
         self.items.append(f\"again {{value}} {{turn}}, {LONG_PIECE}\")\n        self.seen += limits[value]\n\n\
         struct Shelf{id}:\n    pouches: {held}\n\n    \
         def fill(ref self, value: int, limits: list[int]):\n        self.pouches[{at}].add(value, limits=limits)\n\n    \
         def by_hand(ref self, value: int, limits: list[int]):\n        mut pouch = self.pouches[{at}]\n        \
         for turn in range({rounds}):\n            pouch.items.append(f\"by hand {{value}} {{turn}}, {LONG_PIECE}\")\n        \
         pouch.seen += limits[value]\n        \
         self.pouches[{at}] = pouch\n\n\
         def added{id}(ref pouches: {held}, value: int, limits: list[int]):\n    \
         pouches[{at}].add(value, limits=limits)\n\n\
         def by_hand{id}(ref pouches: {held}, value: int, limits: list[int]):\n    mut pouch = pouches[{at}]\n    \
         for turn in range({rounds}):\n        pouch.items.append(f\"by hand {{value}} {{turn}}, {LONG_PIECE}\")\n    \
         pouch.seen += limits[value]\n    \
         pouches[{at}] = pouch\n\n"
    );
    // Each way of changing the pouch, as the step's `how` picks it, through a shelf named `held`,
    // each line at `indent`.
    let through_shelf = |indent: &str| -> String {
        [
            "if how == 0:\n    held.fill(value, limits=limits)".to_owned(),
            "if how == 1:\n    held.by_hand(value, limits=limits)".to_owned(),
            format!("if how == 2:\n    held.pouches[{at}].add(value, limits=limits)"),
            format!("if how == 3:\n    added{id}(ref held.pouches, value=value, limits=limits)"),
            format!("if how == 4:\n    by_hand{id}(ref held.pouches, value=value, limits=limits)"),
        ]
        .iter()
        .flat_map(|lines| lines.lines().map(|line| format!("{indent}{line}\n")).collect::<Vec<_>>())
        .collect()
    };
    match place {
        ShelfPlace::Locked => format!(
            "{types}def filled{id}(shelf: shared Shelf{id}, value: int, limits: list[int], how: int):\n    \
             lock shelf as ref held:\n{}\n\
             def tried{id}(shelf: shared Shelf{id}, value: int, how: int) fails Crashed:\n    limits = [1, 2]\n    \
             with launch(() => filled{id}(shelf, value, limits=limits, how=how)) as task:\n        pass\n\n\
             def shelf_shown{id}(shelf: shared Shelf{id}) -> str:\n    lock shelf as held:\n        \
             return f\"{{held.pouches}}\"\n\n\
             def shelved{id}(stdout: Stdout):\n    shared shelf = Shelf{id}(pouches={made})\n    \
             for value, how in [{steps}]:\n        tried{id}(shelf, value=value, how=how) except Crashed():\n            \
             stdout.print(\"crashed\")\n        stdout.print(shelf_shown{id}(shelf))\n    return\n\n",
            through_shelf("        ")
        ),
        ShelfPlace::Owned | ShelfPlace::Bare => {
            let (making, changing, shown) = if place == ShelfPlace::Owned {
                (format!("mut held = Shelf{id}(pouches={made})"), through_shelf("    "), "held.pouches")
            } else {
                (
                    format!("mut pouches = {made}"),
                    format!(
                        "    if how == 0 or how == 2:\n        pouches[{at}].add(value, limits=limits)\n    \
                         if how == 1:\n        mut pouch = pouches[{at}]\n        \
                         for turn in range({rounds}):\n            \
                         pouch.items.append(f\"by hand {{value}} {{turn}}, {LONG_PIECE}\")\n        \
                         pouch.seen += limits[value]\n        pouches[{at}] = pouch\n    \
                         if how == 3:\n        added{id}(ref pouches, value=value, limits=limits)\n    \
                         if how == 4:\n        by_hand{id}(ref pouches, value=value, limits=limits)\n"
                    ),
                    "pouches",
                )
            };
            format!(
                "{types}def filled{id}(value: int, limits: list[int], how: int, stdout: Stdout):\n    \
                 {making}\n{changing}    stdout.print(f\"{{{shown}}}\")\n\n\
                 def tried{id}(value: int, how: int, stdout: Stdout) fails Crashed:\n    limits = [1, 2]\n    \
                 with launch(() => filled{id}(value, limits=limits, how=how, stdout=stdout)) as task:\n        pass\n\n\
                 def shelved{id}(stdout: Stdout):\n    for value, how in [{steps}]:\n        \
                 tried{id}(value, how=how, stdout=stdout) except Crashed():\n            \
                 stdout.print(\"crashed\")\n    return\n\n"
            )
        }
    }
}

/// `listened{id}`, the function a `Listened` calls, after the functions it uses.
/// `narrowed{id}`, the function a `Narrowed` calls, after its two structs and the functions it
/// uses: `shown{id}`, which prints either member, and for some ways `made{id}`, which gives back
/// a member the checker can't narrow by what it was given, `Holder{id}`, `left{id}`,
/// `grow_crate{id}` and `grow_heap{id}`, or `swapped{id}`.
fn narrowed_written(narrowing: &Narrowing) -> String {
    let Narrowing { id, way, crate_first, n } = *narrowing;
    let union = format!("Crate{id} | Heap{id}");
    let (start, other) = if crate_first {
        (format!("Crate{id}(items=[{n}])"), format!("Heap{id}(held={})", n + 1))
    } else {
        (format!("Heap{id}(held={n})"), format!("Crate{id}(items=[{}])", n + 1))
    };
    let member = if crate_first { "Crate" } else { "Heap" };
    let made = format!("made{id}(crate={}, n={n})", if crate_first { "True" } else { "False" });
    let mut out = format!(
        "struct Crate{id}:\n    items: list[int]\n\n    def grow(ref self) -> int:\n        \
         self.items.append(len(self.items) + 7)\n        return len(self.items)\n\n\
         struct Heap{id}:\n    held: int\n\n    def grow(ref self) -> int:\n        \
         self.held += 3\n        return self.held\n\n\
         def shown{id}(shape: {union}) -> str:\n    match shape:\n        \
         case Crate{id}(items=_):\n            return f\"crate {{shape.items}}\"\n        \
         case Heap{id}(held=_):\n            return f\"heap {{shape.held}}\"\n\n"
    );
    if matches!(
        way,
        NarrowWay::Is
            | NarrowWay::Left
            | NarrowWay::Whole
            | NarrowWay::Matched
            | NarrowWay::RefMember
            | NarrowWay::RefUnion
    ) {
        let _ = write!(
            out,
            "def made{id}(crate: bool, n: int) -> {union}:\n    if crate:\n        \
             return Crate{id}(items=[n])\n    return Heap{id}(held=n)\n\n"
        );
    }
    match way {
        NarrowWay::Field => {
            let _ = write!(out, "struct Holder{id}:\n    shape: {union}\n\n");
        }
        NarrowWay::Left => {
            let _ = write!(
                out,
                "def left{id}(ref shape: {union}) -> int:\n    if shape is Heap{id}:\n        \
                 return shape.grow()\n    return shape.grow() + 100\n\n"
            );
        }
        NarrowWay::RefMember => {
            let _ = write!(
                out,
                "def grow_crate{id}(ref crate: Crate{id}) -> int:\n    return crate.grow()\n\n\
                 def grow_heap{id}(ref heap: Heap{id}) -> int:\n    return heap.grow()\n\n"
            );
        }
        NarrowWay::RefUnion | NarrowWay::RefUnionGrown => {
            let _ = write!(
                out,
                "def swapped{id}(ref shape: {union}, to_heap: bool) -> int:\n    grown = shape.grow()\n    \
                 if to_heap:\n        shape = Heap{id}(held=grown)\n    else:\n        \
                 shape = Crate{id}(items=[grown])\n    return grown\n\n"
            );
        }
        _ => {}
    }
    let to_heap = if crate_first { "True" } else { "False" };
    let lines: Vec<String> = match way {
        NarrowWay::Given => vec![
            format!("mut shape: {union} = {start}"),
            "stdout.print(shape.grow())".to_owned(),
            "stdout.print(shape.grow())".to_owned(),
            format!("stdout.print(shown{id}(shape))"),
            format!("shape = {other}"),
            "stdout.print(shape.grow())".to_owned(),
            format!("stdout.print(shown{id}(shape))"),
        ],
        NarrowWay::Is => vec![
            format!("mut shape = {made}"),
            format!("if shape is Crate{id}:"),
            "    stdout.print(shape.grow(), shape.items)".to_owned(),
            format!("if shape is Heap{id}:"),
            "    stdout.print(shape.grow(), shape.held)".to_owned(),
            format!("stdout.print(shown{id}(shape))"),
        ],
        NarrowWay::Left => vec![
            format!("mut shape = {made}"),
            format!("stdout.print(left{id}(ref shape))"),
            format!("stdout.print(shown{id}(shape))"),
        ],
        NarrowWay::Whole => vec![
            format!("mut shape = {made}"),
            "stdout.print(shape.grow())".to_owned(),
            "stdout.print(shape.grow())".to_owned(),
            format!("stdout.print(shown{id}(shape))"),
        ],
        NarrowWay::Matched => vec![
            format!("mut shape = {made}"),
            "match shape:".to_owned(),
            format!("    case Crate{id}(items=_):"),
            "        stdout.print(shape.grow())".to_owned(),
            format!("    case Heap{id}(held=_):"),
            "        stdout.print(shape.grow())".to_owned(),
            format!("stdout.print(shown{id}(shape))"),
        ],
        NarrowWay::ForRef => vec![
            format!("mut shapes: list[{union}] = [{start}, {other}]"),
            "for ref each in shapes:".to_owned(),
            "    stdout.print(each.grow())".to_owned(),
            format!("    if each is Crate{id}:"),
            "        stdout.print(each.grow())".to_owned(),
            "for each in shapes:".to_owned(),
            format!("    stdout.print(shown{id}(each))"),
        ],
        NarrowWay::Field => vec![
            format!("mut holder = Holder{id}(shape={start})"),
            "stdout.print(holder.shape.grow())".to_owned(),
            "stdout.print(holder.shape.grow())".to_owned(),
            format!("stdout.print(shown{id}(holder.shape))"),
        ],
        NarrowWay::RefMember => {
            let grow = if crate_first { "grow_crate" } else { "grow_heap" };
            vec![
                format!("mut shape = {made}"),
                format!("if shape is Crate{id}:"),
                format!("    stdout.print(grow_crate{id}(ref shape))"),
                format!("if shape is Heap{id}:"),
                format!("    stdout.print(grow_heap{id}(ref shape))"),
                format!("stdout.print(shown{id}(shape))"),
                format!("mut given: {union} = {start}"),
                format!("stdout.print({grow}{id}(ref given))"),
                format!("stdout.print(shown{id}(given))"),
            ]
        }
        NarrowWay::RefUnion => vec![
            format!("mut shape = {made}"),
            format!("if shape is {member}{id}:"),
            format!("    stdout.print(swapped{id}(ref shape, to_heap={to_heap}))"),
            format!("stdout.print(shown{id}(shape))"),
            "stdout.print(shape.grow())".to_owned(),
            format!("stdout.print(shown{id}(shape))"),
        ],
        NarrowWay::RefUnionGrown => vec![
            format!("mut shape: {union} = {start}"),
            format!("stdout.print(swapped{id}(ref shape, to_heap={to_heap}))"),
            "stdout.print(shape.grow())".to_owned(),
            format!("stdout.print(shown{id}(shape))"),
        ],
    };
    let _ = writeln!(out, "def narrowed{id}(stdout: Stdout):");
    for line in lines {
        let _ = writeln!(out, "    {line}");
    }
    out.push('\n');
    out
}

/// The value a `RefCased` starts from, of type `ty` (`Named` or `Held`), as `REF_STARTS` counts
/// them.
fn ref_start_written(start: usize, ty: &str) -> String {
    match start {
        0 => format!("{ty}.Leaf(name=\"a\")"),
        1 => format!("{ty}.Pair(left=.Leaf(name=\"a\"), right=.Leaf(name=\"b\"), label=\"x\", tags=[])"),
        _ => format!(
            "{ty}.Pair(left=.Pair(left=.Leaf(name=\"a\"), right=.Leaf(name=\"b\"), label=\"in\", tags=[]), \
             right=.Leaf(name=\"c\"), label=\"out\", tags=[\"t\"])"
        ),
    }
}

/// `ref_cased{id}`, the function a `RefCased` calls, and for `Shadowed`, `ref_shadowed{id}`, whose
/// parameter `at` is the index of what its `match` is over and the `ref` name of each case too.
/// The two ways that hold their value in a list hold a `Held`, `Named`'s twin: a type held in a
/// list isn't made of native blocks, and `Named` has to be wherever there are no other threads.
fn ref_cased_written(casing: &RefCasing) -> String {
    let RefCasing { id, way, start, text } = *casing;
    let start = ref_start_written(start, if way.holds_in_a_list() { "Held" } else { "Named" });
    let text = REF_TEXTS.get(text).copied().unwrap_or_default();
    let mut out = String::new();
    if way == RefWay::Shadowed {
        let _ = write!(
            out,
            "def ref_shadowed{id}(ref row: list[Held], at: int):\n    match row[at]:\n        \
             case .Pair(_, _, ref at, _):\n            at += \"{text}\"\n        \
             case .Leaf(ref at):\n            at += \"{text}\"\n\n"
        );
    }
    let failed = match way {
        RefWay::ForRefWhile => Some(format!("while ref_failing{id}(len(tag))?:\n                    pass")),
        RefWay::ForRefElif => Some(format!(
            "if len(tag) == 0:\n                    pass\n                elif ref_failing{id}(len(tag))?:\n                    pass"
        )),
        RefWay::ForRefAnd => {
            Some(format!("if len(tag) > 0 and ref_failing{id}(len(tag))?:\n                    pass"))
        }
        _ => None,
    };
    if let Some(failed) = &failed {
        let _ = write!(
            out,
            "def ref_failing{id}(count: int) -> bool fails Refused:\n    if count > 0:\n        \
             fail Refused(code=count)\n    return False\n\n\
             def ref_forred{id}(ref named: Named) fails Refused:\n    match named:\n        \
             case .Pair(_, _, _, ref tags):\n            for ref tag in tags:\n                \
             tag += \"{text}\"\n                {failed}\n        \
             case .Leaf(ref name):\n            name += \"{text}\"\n\n"
        );
    }
    let _ = writeln!(out, "def ref_cased{id}(stdout: Stdout):");
    let lines: Vec<String> = match way {
        RefWay::Deep => vec![
            format!("mut named = {start}"),
            "match named:".to_owned(),
            "    case .Pair(left=ref left, tags=_):".to_owned(),
            "        match left:".to_owned(),
            "            case .Pair(left=ref inner, tags=_):".to_owned(),
            "                match inner:".to_owned(),
            "                    case .Leaf(ref name):".to_owned(),
            format!("                        name += \"{text}\""),
            "                    case _:".to_owned(),
            "                        pass".to_owned(),
            "            case .Leaf(ref name):".to_owned(),
            format!("                name += \"{text}\""),
            "    case .Leaf(ref name):".to_owned(),
            format!("        name += \"{text}\""),
            "stdout.print(named_digest(named))".to_owned(),
        ],
        RefWay::NewValue => vec![
            format!("mut named = {start}"),
            "match named:".to_owned(),
            "    case .Pair(label=_, left=ref left):".to_owned(),
            format!("        left = .Leaf(name=\"{text}\")"),
            "    case .Leaf(ref name):".to_owned(),
            format!("        name = \"{text}\""),
            "stdout.print(named_digest(named))".to_owned(),
        ],
        RefWay::Shadowed => vec![
            format!("mut row = [{start}, Held.Leaf(name=\"z\")]"),
            format!("ref_shadowed{id}(ref row, 0)"),
            "stdout.print(held_digest(row[0]), held_digest(row[1]))".to_owned(),
        ],
        RefWay::Guarded => vec![
            format!("mut named = {start}"),
            "match named:".to_owned(),
            "    case .Pair(_, _, ref label, ref tags) if len(label) < 3:".to_owned(),
            format!("        label += \"{text}\""),
            "        tags.append(label)".to_owned(),
            "    case .Leaf(ref name) if len(name) > 0:".to_owned(),
            format!("        name += \"{text}\""),
            "    case _:".to_owned(),
            "        pass".to_owned(),
            "stdout.print(named_digest(named))".to_owned(),
        ],
        RefWay::IndexMoved => vec![
            format!("mut row = [{start}, Held.Leaf(name=\"z\")]"),
            "mut at = 0".to_owned(),
            "match row[at]:".to_owned(),
            "    case .Pair(_, _, ref label, _):".to_owned(),
            "        at += 1".to_owned(),
            format!("        label += \"{text}\""),
            "    case .Leaf(ref name):".to_owned(),
            "        at += 1".to_owned(),
            format!("        name += \"{text}\""),
            "stdout.print(held_digest(row[0]), held_digest(row[1]), at)".to_owned(),
        ],
        RefWay::ForRefWhile | RefWay::ForRefElif | RefWay::ForRefAnd => vec![
            format!("mut named = {start}"),
            format!("ref_forred{id}(ref named) except why:"),
            "    stdout.print(\"refused\", why.code)".to_owned(),
            "stdout.print(named_digest(named))".to_owned(),
        ],
    };
    for line in lines {
        let _ = writeln!(out, "    {line}");
    }
    out.push('\n');
    out
}

/// `captured{id}`, the function a `Captured` calls, with what it calls. Each prints what the name
/// written there means, and for a `lock`, what the lock's value holds after.
fn captured_written(capturing: &Capturing) -> String {
    let Capturing { id, way, n } = *capturing;
    let bag = |items: &str| format!("Bag(items=[{items}], counts=dict[str, int]())");
    let locked = |inside: &[String]| {
        let mut out = format!(
            "def captured{id}(stdout: Stdout):\n    shared bag = {}\n    lock bag as ref held:\n",
            bag("1, 2")
        );
        for line in inside {
            let _ = writeln!(out, "        {line}");
        }
        let _ = write!(
            out,
            "        held.items.append({n})\n    lock bag as held:\n        stdout.print(held.items)\n\n"
        );
        out
    };
    match way {
        CaptureWay::WithDef => format!(
            "def opened{id}() -> with int:\n    lend {n}\n\ndef captured{id}(stdout: Stdout):\n    \
             with opened{id}() as added:\n        def doubled(added: int) -> int:\n            \
             return added * 2\n        stdout.print(doubled(100), added)\n\n"
        ),
        CaptureWay::LockLoop => locked(&[
            format!("for held in [{}]:", bag("7, 8, 9")),
            "    stdout.print(len(held.items))".to_owned(),
        ]),
        CaptureWay::LockCase => locked(&[
            format!("pair = ({}, 0)", bag("5, 6, 7, 8")),
            "match pair:".to_owned(),
            "    case (held, _):".to_owned(),
            "        stdout.print(len(held.items))".to_owned(),
        ]),
        CaptureWay::LockDef => locked(&[
            "def counted(held: Bag) -> int:".to_owned(),
            "    return len(held.items)".to_owned(),
            format!("stdout.print(counted({}))", bag("4, 4, 4, 4, 4")),
        ]),
        CaptureWay::LockArrow => locked(&[
            "measure: (Bag) -> int = held => len(held.items)".to_owned(),
            format!("stdout.print(measure({}))", bag("3, 3, 3")),
        ]),
        CaptureWay::SourceCall => format!(
            "def total{id}() -> int:\n    return {n}\n\ndef counted{id}() -> with int:\n    \
             lend total{id}()\n\ndef capturing{id}(total{id}: () -> int, stdout: Stdout):\n    \
             with counted{id}() as got:\n        stdout.print(got, total{id}())\n\n\
             def captured{id}(stdout: Stdout):\n    capturing{id}(() => {n} + 2, stdout)\n\n"
        ),
    }
}

fn listened_written(listening: &Listening) -> String {
    let Listening { id, size, early, sent, dropping, ref inboxes } = *listening;
    let mut out = format!(
        "def read{id}(hits: Inbox[int], count: int, looping: bool, stdout: Stdout):\n    \
         mut got: list[int] = []\n    if looping:\n        for hit in hits:\n            got.append(hit)\n            \
         if len(got) == count:\n                break\n        stdout.print(f\"looped {{got}}\")\n        return\n    \
         for _ in range(count):\n        got.append(hits.receive() else -1)\n    stdout.print(f\"took {{got}}\")\n\n\
         def reading{id}(hits: Inbox[int], count: int, looping: bool, stdout: Stdout) fails Crashed:\n    \
         with launch(() => read{id}(hits, count=count, looping=looping, stdout=stdout)) as task:\n        pass\n\n"
    );
    if dropping {
        let _ = write!(
            out,
            "def dropping{id}(hits: Channel[int], replies: Sender[int], gate: Channel[int], last: int, stdout: Stdout) \
             fails Closed:\n    mut waited = False\n    mut got: list[int] = []\n    \
             for hit in hits.listen(when_behind=.DropOldest):\n        if hit < 0:\n            if not waited:\n                \
             replies.send(-1)?\n                waited = True\n                _ = gate.receive() else fail Closed()\n            \
             continue\n        got.append(hit)\n        if hit == last:\n            break\n    \
             stdout.print(f\"listened {{got}}\")\n    replies.send(0)?\n\n\
             def until_heard{id}(hits: Channel[int], replies: Channel[int]) fails Closed | Timeout:\n    while True:\n        \
             hits.send(-1)?\n        heard = replies.receive_within(2ms) except Timeout(): continue\n        \
             if heard is not None:\n            return\n\n"
        );
    }
    let (takes, passes) = if dropping {
        (
            "hits: Channel[int], replies: Channel[int], gate: Channel[int], stdout: Stdout",
            "hits, replies=replies, gate=gate, stdout=stdout",
        )
    } else {
        ("hits: Channel[int], stdout: Stdout", "hits, stdout=stdout")
    };
    let _ = writeln!(out, "def heard{id}({takes}) fails Closed:");
    for at in 0..inboxes.len() {
        let _ = writeln!(out, "    inbox{at} = hits.inbox()");
    }
    let _ = writeln!(out, "    for hit in range(1, {sent} + 1):\n        hits.send(hit)?");
    if dropping {
        out.push_str("    gate.send(1)?\n    _ = replies.receive() else fail Closed()\n");
    }
    for (at, looping) in inboxes.iter().enumerate() {
        let looping = if *looping { "True" } else { "False" };
        let _ = writeln!(
            out,
            "    reading{id}(inbox{at}, count={sent}, looping={looping}, stdout=stdout) except Crashed():\n        \
             stdout.print(\"behind\")"
        );
    }
    let _ = write!(
        out,
        "    return\n\ndef listened{id}(stdout: Stdout) fails Closed | Crashed | Timeout:\n    \
         with channel[int](size={size}, to=.Every) as hits:\n"
    );
    let mut indent = "        ".to_owned();
    if dropping {
        let _ = write!(
            out,
            "{indent}with channel[int](size=4) as replies:\n{indent}    with channel[int](size=1) as gate:\n"
        );
        indent.push_str("        ");
    }
    if early {
        let _ = writeln!(out, "{indent}hits.send(100)?");
    }
    if dropping {
        let _ = write!(
            out,
            "{indent}with launch(\n{indent}    () => dropping{id}(\n{indent}        hits,\n{indent}        \
             replies=replies.sender(),\n{indent}        gate=gate,\n{indent}        last={sent},\n{indent}        \
             stdout=stdout,\n{indent}    )?,\n{indent})?:\n{indent}    until_heard{id}(hits, replies=replies)?\n{indent}    "
        );
    } else {
        out.push_str(&indent);
    }
    let _ = write!(out, "heard{id}({passes})?\n\n");
    out
}

/// A piece of text past the 23 bytes a str holds in place, with letters past ASCII.
pub(crate) const LONG_PIECE: &str = "a piece of text past 23 bytes, é and 🦊, ";

/// A list of ints as Fennec writes one.
fn ints_written(items: &[i64]) -> String {
    let items: Vec<String> = items.iter().map(ToString::to_string).collect();
    format!("[{}]", items.join(", "))
}

/// A `Holder` as Fennec writes one.
fn holder_written((items, tag): &(Vec<i64>, i64)) -> String {
    format!("Holder(items={}, tag={tag})", ints_written(items))
}

/// A dict as Fennec writes one, from its keys and how each value is written.
fn dict_written<T>(pairs: &[(String, T)], value: impl Fn(&T) -> String) -> String {
    let pairs: Vec<String> = pairs.iter().map(|(key, held)| format!("\"{key}\": {}", value(held))).collect();
    format!("{{{}}}", pairs.join(", "))
}

/// The type an `Aliased` task of `aliased{id}` is handed, for what it holds, and the name its
/// helper ends with.
fn handed_type(id: usize, held: Held) -> (String, &'static str) {
    let (ty, suffix) = match held {
        Held::Numbers => ("list[int]", "numbers"),
        Held::Holders => ("list[Holder]", "holders"),
        Held::Holder => ("Holder", "holder"),
        Held::Lists => ("dict[str, list[int]]", "lists"),
        Held::Spots => ("dict[str, Point]", "spots"),
        Held::Keepers => ("dict[str, Holder]", "keepers"),
        Held::Counts => ("dict[str, int]", "counts"),
        Held::Ledger => return (format!("Ledger{id}"), "ledger"),
        Held::Rooms => return (format!("Table[Room{id}]"), "rooms"),
        Held::Table | Held::Entries | Held::Point | Held::Text => ("Table[Point]", "table"),
    };
    (ty.to_owned(), suffix)
}

/// What a task handed `mine`, as `held` holds it, does to its own copy, then what it gives back.
fn handed_body(held: Held) -> &'static str {
    match held {
        Held::Numbers => "    mine.append(n)\n    mine[0] = n\n    return sum(mine)\n",
        Held::Holders => {
            "    mine[0].items.append(n)\n    mine[1].tag = n\n    return len(mine[0].items) + mine[1].tag\n"
        }
        Held::Holder => {
            "    mine.items.append(n)\n    mine.tag += n\n    return len(mine.items) + mine.tag\n"
        }
        Held::Lists => {
            "    mine[\"a\"].append(n)\n    mine[\"b\"][0] = n\n    return len(mine[\"a\"]) + mine[\"b\"][0]\n"
        }
        Held::Spots => {
            "    mine[\"a\"].x += n\n    mine[\"b\"].moved(n)\n    return mine[\"a\"].x + mine[\"b\"].x\n"
        }
        Held::Keepers => {
            "    mine[\"a\"].items.append(n)\n    mine[\"b\"].tag = n\n    return len(mine[\"a\"].items) + mine[\"b\"].tag\n"
        }
        Held::Counts => "    mine[\"a\"] += n\n    return sum(mine.values())\n",
        Held::Ledger => {
            "    for ref point in mine.points:\n        point.x += n\n    mine.count += 1\n    \
             return mine.count + sum(point.x for point in mine.points)\n"
        }
        Held::Rooms => {
            "    mut total = 0\n    for ref room in mine:\n        room.counts[\"a\"] += n\n        \
             total += sum(room.counts.values())\n    return total\n"
        }
        Held::Table | Held::Entries | Held::Point | Held::Text => {
            "    for ref point in mine:\n        point.x += n\n    return sum(point.x for point in mine)\n"
        }
    }
}

/// A line printing a walk of `from`, which holds `held`: the sum of something in each of its
/// values, its keys and its items, or each room's dict summed and each room's label.
fn walk_written(from: &str, held: Held) -> String {
    let (value, summed, shown) = match held {
        Held::Lists => ("items", "len(items)", "len(items)"),
        Held::Spots => ("point", "point.x", "point.y"),
        Held::Keepers => ("holder", "holder.tag", "len(holder.items)"),
        Held::Rooms => {
            return format!(
                "    stdout.print(\"walk\", [sum(room.counts.values()) for room in {from}], [room.label for room in \
                 {from}])\n"
            );
        }
        _ => {
            return format!(
                "    stdout.print(\"walk\", sum({from}.values()), [key for key in {from}.keys()], \
                 [f\"{{key}}={{count}}\" for key, count in {from}.items()])\n"
            );
        }
    };
    format!(
        "    stdout.print(\"walk\", sum({summed} for {value} in {from}.values()), [key for key in {from}.keys()], \
         [f\"{{key}}={{{shown}}}\" for key, {value} in {from}.items()])\n"
    )
}

/// `aliased{id}`, the function an `Aliased` calls, after the structs and helpers it uses: copies
/// made, each name changed by its steps, then every name printed.
fn aliased_written(aliasing: &Aliasing) -> String {
    let Aliasing { id, base, steps } = aliasing;
    let held = aliasing.names();
    let changed: Vec<usize> = steps
        .iter()
        .filter_map(|step| match step {
            Alias::Written { to, .. }
            | Alias::Looped(Looping { to, .. })
            | Alias::Read { from: to, how: Reading::Replaced, .. } => Some(*to),
            _ => None,
        })
        .collect();
    let mutable =
        |n: usize| if n == TABLE || n == LEDGER || n == ROOMS || changed.contains(&n) { "mut " } else { "" };
    let mut out = format!(
        "struct Ledger{id}:\n    points: Table[Point]\n    count: int\n\nstruct Room{id}:\n    counts: dict[str, \
         int]\n    label: str\n\n    def labeled(ref self, first: str, second: str):\n        self.label += first + \
         second\n\n"
    );
    if steps.iter().any(|step| matches!(step, Alias::Written { how: Writing::Ref, .. })) {
        let _ = write!(
            out,
            "def poked{id}(ref items: list[int], n: int):\n    items.append(n)\n    items[0] += n\n    return\n\n"
        );
    }
    if steps.iter().any(|step| matches!(step, Alias::Read { how: Reading::Returned, .. })) {
        let _ = write!(
            out,
            "def first_over{id}(table: Table[Point], n: int) -> Point?:\n    for point in table:\n        if point.x > n:\n            \
             return point\n    return None\n\n"
        );
    }
    if steps.iter().any(|step| matches!(step, Alias::Read { how: Reading::Failed, .. })) {
        let _ = write!(
            out,
            "def refused_over{id}(table: Table[Point], n: int) -> int fails Refused:\n    mut total = 0\n    for point in table:\n        \
             if point.x > n:\n            fail Refused(code=point.x)\n        total += point.y\n    return total\n\n"
        );
    }
    let mut handed: Vec<&str> = Vec::new();
    for step in steps {
        if let Alias::Tasked { from } = step {
            let kind = held.get(*from).copied().unwrap_or(Held::Numbers);
            let (ty, suffix) = handed_type(*id, kind);
            if !handed.contains(&suffix) {
                handed.push(suffix);
                let _ = write!(
                    out,
                    "def handed{id}_{suffix}(given: {ty}, n: int) -> int:\n    mut mine = given\n{}\n",
                    handed_body(kind)
                );
            }
        }
    }
    let _ = writeln!(out, "def aliased{id}(stdout: Stdout):");
    for (n, value) in starting(*base).iter().enumerate() {
        let name = alias_name(n);
        let written = match value {
            Kept::Numbers(items) => ints_written(items),
            Kept::Holders(holders) => {
                let holders: Vec<String> = holders.iter().map(holder_written).collect();
                format!("[{}]", holders.join(", "))
            }
            Kept::Lists(lists) => dict_written(lists, |items| ints_written(items)),
            Kept::Spots(spots) => dict_written(spots, |(x, y)| format!("Point(x={x}, y={y})")),
            Kept::Keepers(holders) => dict_written(holders, holder_written),
            Kept::Counts(counts) => dict_written(counts, ToString::to_string),
            Kept::Table(points) => {
                let _ = writeln!(out, "    mut {name} = Table[Point]()");
                for (point, (x, y)) in ["first_id", "second_id"].iter().zip(points) {
                    let _ = writeln!(out, "    {point} = {name}.add(Point(x={x}, y={y}))");
                }
                continue;
            }
            Kept::Ledger(points, count) => {
                let _ = writeln!(out, "    mut {name} = Ledger{id}(points=Table[Point](), count={count})");
                for (entry, (x, y)) in ["entry_a", "entry_b"].iter().zip(points) {
                    let _ = writeln!(out, "    {entry} = {name}.points.add(Point(x={x}, y={y}))");
                }
                continue;
            }
            Kept::Rooms(rooms) => {
                let _ = writeln!(out, "    mut {name} = Table[Room{id}]()");
                for (room, (counts, label)) in ["room_a", "room_b"].iter().zip(rooms) {
                    let counts = dict_written(counts, ToString::to_string);
                    let _ = writeln!(
                        out,
                        "    {room} = {name}.add(Room{id}(counts={counts}, label=\"{label}\"))"
                    );
                }
                continue;
            }
            Kept::Holder(..) | Kept::Point(..) | Kept::Entries(_) => continue,
            Kept::Text(text) => format!("\"{text}\""),
        };
        let _ = writeln!(out, "    {}{name} = {written}", mutable(n));
    }
    let mut made = STARTING.len();
    for (s, step) in steps.iter().enumerate() {
        match *step {
            Alias::Copied { from } => {
                let _ = writeln!(out, "    {}{} = {}", mutable(made), alias_name(made), alias_name(from));
                made += 1;
            }
            Alias::Part { from, at } => {
                let part = part_written(&alias_name(from), held.get(from).copied(), at);
                let _ = writeln!(out, "    {}{} = {part}", mutable(made), alias_name(made));
                made += 1;
            }
            Alias::Built { from, n } => {
                let _ = writeln!(
                    out,
                    "    spare{s} = {}\n    {}{} = Holder(items=spare{s}, tag={n})",
                    alias_name(from),
                    mutable(made),
                    alias_name(made)
                );
                made += 1;
            }
            Alias::Written { to, how, at, n } => {
                let kind = held.get(to).copied().unwrap_or(Held::Numbers);
                out.push_str(&written_change(*id, &alias_name(to), kind, (how, at, n), s));
            }
            Alias::Looped(looping) => {
                let kind = held.get(looping.to).copied().unwrap_or(Held::Holders);
                out.push_str(&looped_written(*id, &alias_name(looping.to), kind, looping, s));
            }
            Alias::Tasked { from } => {
                let (_, suffix) = handed_type(*id, held.get(from).copied().unwrap_or(Held::Numbers));
                let _ = writeln!(
                    out,
                    "    sums{s} = [handed{id}_{suffix}({}, n=i) for i in parallel(range(3))]\n    \
                     stdout.print(\"sums\", sums{s})",
                    alias_name(from)
                );
            }
            Alias::Walked { from } => {
                out.push_str(&walk_written(
                    &alias_name(from),
                    held.get(from).copied().unwrap_or(Held::Counts),
                ));
            }
            Alias::Read { from, how, n } => {
                let kind = held.get(from).copied().unwrap_or(Held::Table);
                let name = alias_name(from);
                let table = if kind == Held::Ledger { format!("{name}.points") } else { name };
                if how == Reading::Kept {
                    let kept = alias_name(made);
                    made += 1;
                    let _ = write!(
                        out,
                        "    mut {kept} = Point(x=0, y=0)\n    for point in {table}:\n        if point.x >= {n}:\n            \
                         {kept} = point\n"
                    );
                } else {
                    out.push_str(&read_written(*id, &table, kind == Held::Rooms, how, n, s));
                }
            }
            Alias::Drained { from, at, n } => {
                let fresh = fresh_written(held.get(from).copied().unwrap_or(Held::Counts), n);
                let _ = writeln!(
                    out,
                    "    mut last{s} = {}\n    last{s}[\"{}\"] = {fresh}\n    stdout.print(\"values\", last{s}.values())",
                    alias_name(from),
                    key_written(at)
                );
            }
        }
    }
    let _ = writeln!(out, "    stdout.print(\"ids\", table[first_id], table[second_id])");
    let _ = writeln!(out, "    stdout.print(\"entries\", ledger.points[entry_a], ledger.points[entry_b])");
    for n in 0..made {
        let _ = writeln!(out, "    stdout.print(\"{0}\", {0})", alias_name(n));
    }
    out.push_str("    return\n\n");
    out
}

/// A part of `from`, which holds `held`: its item `at`, its value at key `at`, its point with
/// `Id` `at`, or a field holding a collection.
fn part_written(from: &str, held: Option<Held>, at: usize) -> String {
    match held {
        Some(Held::Holders) => format!("{from}[{at}]"),
        Some(Held::Holder) => format!("{from}.items"),
        Some(Held::Ledger) => format!("{from}.points"),
        Some(Held::Table) => format!("{from}[{}]", id_written(at)),
        Some(Held::Entries) => format!("{from}[{}]", entry_written(at)),
        Some(Held::Rooms) => format!("{from}[{}].counts", room_written(at)),
        _ => format!("{from}[\"{}\"]", key_written(at)),
    }
}

fn key_written(at: usize) -> &'static str {
    KEYS.get(at).copied().unwrap_or("a")
}

fn id_written(at: usize) -> &'static str {
    if at == 0 { "first_id" } else { "second_id" }
}

fn entry_written(at: usize) -> &'static str {
    if at == 0 { "entry_a" } else { "entry_b" }
}

fn room_written(at: usize) -> &'static str {
    if at == 0 { "room_a" } else { "room_b" }
}

/// A `Read` step's loop over `table`, a table of rooms when `rooms`, read as `how`, as step `s`
/// of `aliased{id}`. `Reading::Kept` is written where it makes its name.
fn read_written(id: usize, table: &str, rooms: bool, how: Reading, n: i64, s: usize) -> String {
    match (rooms, how) {
        (true, Reading::Broken) => format!(
            "    mut total{s} = 0\n    for room in {table}:\n        if sum(room.counts.values()) > {n}:\n            break\n        \
             total{s} += len(room.label)\n    stdout.print(\"broke\", total{s})\n"
        ),
        (true, _) => format!(
            "    mut total{s} = 0\n    for room in {table}:\n        total{s} += sum(room.counts.values()) + len(room.label)\n    \
             stdout.print(\"read\", total{s})\n"
        ),
        (false, Reading::Replaced) => format!(
            "    mut spare{s} = {table}\n    for ref point in spare{s}:\n        point.y += 0\n    mut swapped{s} = False\n    \
             for point in {table}:\n        if not swapped{s}:\n            {table} = spare{s}\n            swapped{s} = True\n        \
             stdout.print(\"item\", point.x, point.y)\n"
        ),
        (false, Reading::Broken) => format!(
            "    mut total{s} = 0\n    for point in {table}:\n        if point.x > {n}:\n            break\n        total{s} += \
             point.y + 1\n    stdout.print(\"broke\", total{s})\n"
        ),
        (false, Reading::Returned) => format!(
            "    found{s} = first_over{id}({table}, n={n}) else Point(x=-1, y=-1)\n    stdout.print(\"first\", found{s})\n"
        ),
        (false, Reading::Failed) => format!(
            "    got{s} = refused_over{id}({table}, n={n}) except Refused(): -1\n    stdout.print(\"refused\", got{s})\n"
        ),
        (false, _) => format!(
            "    mut total{s} = 0\n    for point in {table}:\n        total{s} += point.x * 3 + point.y\n    \
             stdout.print(\"read\", total{s})\n"
        ),
    }
}

/// A fresh value for a dict holding `held`, made from `n`.
fn fresh_written(held: Held, n: i64) -> String {
    match held {
        Held::Lists => format!("[{n}, {n}]"),
        Held::Spots => format!("Point(x={n}, y={n})"),
        Held::Keepers => format!("Holder(items=[{n}, {n}], tag={n})"),
        _ => n.to_string(),
    }
}

/// `_ = to.remove(key)`, a walk of what's left of `to`, which holds `held`, then the key put
/// back with a value made from `n`.
fn removed_written(to: &str, held: Held, at: usize, n: i64) -> String {
    let key = key_written(at);
    let fresh = fresh_written(held, n);
    format!("    _ = {to}.remove(\"{key}\")\n{}    {to}[\"{key}\"] = {fresh}\n", walk_written(to, held))
}

/// What a `Looped` step writes, taking an item out of `to`, which holds `held`, as step `s` of
/// `aliased{id}`.
fn looped_written(id: usize, to: &str, held: Held, looping: Looping, s: usize) -> String {
    let Looping { at, n, turns, between, inside, items, .. } = looping;
    let key = key_written(at);
    // What holds the item, and where the item is in it.
    let (holder, place, id_at) = match held {
        Held::Ledger => {
            (format!("{to}.points"), format!("{to}.points[{}]", entry_written(at)), entry_written(at))
        }
        Held::Entries => (to.to_owned(), format!("{to}[{}]", entry_written(at)), entry_written(at)),
        Held::Table => (to.to_owned(), format!("{to}[{}]", id_written(at)), id_written(at)),
        Held::Rooms => (to.to_owned(), format!("{to}[{}]", room_written(at)), room_written(at)),
        Held::Holders => (to.to_owned(), format!("{to}[{at}]"), ""),
        _ => (to.to_owned(), format!("{to}[\"{key}\"]"), ""),
    };
    let taken = if items { format!("{place}.items") } else { place.clone() };
    let listed = items || held == Held::Lists;
    let holds = matches!(held, Held::Holders | Held::Keepers);
    // The copy changed as a `PutBack` step changes it, then each turn's put-back and exit.
    let copy_changed = |pad: &str| match held {
        _ if listed => format!("{pad}taken{s}.append({n})\n"),
        _ if holds => format!("{pad}taken{s}.tag += {n}\n{pad}taken{s}.items.append({n})\n"),
        Held::Rooms => format!("{pad}taken{s}.counts[\"a\"] += {n}\n"),
        _ => format!("{pad}taken{s}.y += {n}\n"),
    };
    let mut out = format!("    mut turn{s} = 0\n");
    if inside {
        let _ = write!(out, "    while True:\n        mut taken{s} = {taken}\n{}", copy_changed("        "));
    } else {
        let _ = write!(out, "    mut taken{s} = {taken}\n{}    while True:\n", copy_changed("    "));
    }
    let _ = write!(
        out,
        "        {taken} = taken{s}\n        turn{s} += 1\n        if turn{s} == {turns}:\n            break\n"
    );
    let pad = "        ";
    let lines = match between {
        Between::Changed => match held {
            _ if listed => format!("{pad}{taken}.append({n})\n"),
            _ if holds => format!("{pad}{place}.tag = {n}\n{pad}{place}.items.append({n})\n"),
            Held::Rooms => format!("{pad}{place}.counts[\"b\"] = {n}\n"),
            _ => format!("{pad}{place}.x += {n}\n"),
        },
        Between::Grown(count) => {
            let grown_key = format!("{holder}[f\"g{{turn{s}}}x{{more{s}}}\"]");
            let added = match held {
                Held::Table | Held::Entries | Held::Ledger => {
                    format!("_ = {holder}.add(Point(x=more{s}, y={n}))")
                }
                Held::Rooms => format!("_ = {holder}.add(Room{id}(counts={{\"a\": more{s}}}, label=\"g\"))"),
                Held::Holders => format!("{holder}.append(Holder(items=[more{s}], tag={n}))"),
                Held::Keepers => format!("{grown_key} = Holder(items=[more{s}], tag={n})"),
                Held::Lists => format!("{grown_key} = [more{s}]"),
                _ => format!("{grown_key} = Point(x=more{s}, y={n})"),
            };
            format!("{pad}for more{s} in range({count}):\n{pad}    {added}\n")
        }
        Between::Removed => match held {
            Held::Table | Held::Entries | Held::Ledger | Held::Rooms => {
                format!("{pad}_ = {holder}.remove({id_at})\n")
            }
            Held::Holders => {
                format!("{pad}_ = {holder}.pop({at})\n{pad}{holder}.append(Holder(items=[{n}], tag={n}))\n")
            }
            // The next turn reads the item, or puts back only its items, so it has to be there.
            _ if inside || items => {
                let fresh = match held {
                    Held::Keepers => format!("Holder(items=[{n}], tag={n})"),
                    Held::Lists => format!("[{n}]"),
                    _ => format!("Point(x={n}, y={n})"),
                };
                format!("{pad}_ = {holder}.remove(\"{key}\")\n{pad}{holder}[\"{key}\"] = {fresh}\n")
            }
            _ => format!("{pad}_ = {holder}.remove(\"{key}\")\n"),
        },
    };
    out.push_str(&lines);
    out
}

/// What a `Written` step writes, changing `to`, which holds `held`, as step `s` of `aliased{id}`.
fn written_change(id: usize, to: &str, held: Held, (how, at, n): (Writing, usize, i64), s: usize) -> String {
    let key = key_written(at);
    // A table's items by `Id`: the starting table's, or the ledger's.
    let (table, point) = match held {
        Held::Ledger => (format!("{to}.points"), entry_written(at)),
        Held::Entries => (to.to_owned(), entry_written(at)),
        _ => (to.to_owned(), id_written(at)),
    };
    let room = room_written(at);
    match (held, how) {
        (Held::Lists | Held::Spots | Held::Keepers | Held::Counts, Writing::Removed) => {
            removed_written(to, held, at, n)
        }
        (Held::Numbers, Writing::Append) => format!("    {to}.append({n})\n"),
        (Held::Numbers, Writing::Set) => format!("    {to}[{at}] = {n}\n"),
        (Held::Numbers, Writing::ForRef) => format!("    for ref number in {to}:\n        number += {n}\n"),
        (Held::Numbers, _) => format!("    poked{id}(ref {to}, n={n})\n"),
        (Held::Holders, Writing::ForRef) => format!(
            "    for ref holder in {to}:\n        holder.tag += {n}\n        holder.items.append({n})\n"
        ),
        (Held::Holders, Writing::Field) => format!("    {to}[{at}].tag = {n}\n"),
        (Held::Holders, Writing::Append) => format!("    {to}[{at}].items.append({n})\n"),
        (Held::Holders, Writing::PutBack) => format!(
            "    mut taken{s} = {to}[{at}]\n    taken{s}.tag += {n}\n    taken{s}.items.append({n})\n    \
             {to}[{at}] = taken{s}\n"
        ),
        (Held::Holders, _) => format!("    {to}[{at}].grown_by([{n}, {n}])\n"),
        (Held::Keepers, Writing::Field) => format!("    {to}[\"{key}\"].tag = {n}\n"),
        (Held::Keepers, Writing::Append) => format!("    {to}[\"{key}\"].items.append({n})\n"),
        (Held::Keepers, Writing::Set) => format!("    {to}[\"{key}\"].items[0] = {n}\n"),
        (Held::Keepers, Writing::PutBack) => format!(
            "    mut taken{s} = {to}[\"{key}\"]\n    taken{s}.tag += {n}\n    taken{s}.items.append({n})\n    \
             {to}[\"{key}\"] = taken{s}\n"
        ),
        (Held::Keepers, _) => format!("    {to}[\"{key}\"].grown_by([{n}])\n"),
        (Held::Holder, Writing::Field) => format!("    {to}.tag = {n}\n"),
        (Held::Holder, Writing::Append) => format!("    {to}.items.append({n})\n"),
        (Held::Holder, _) => format!("    {to}.grown_by([{n}])\n"),
        (Held::Lists, Writing::Set) => format!("    {to}[\"{key}\"][0] = {n}\n"),
        (Held::Lists, Writing::PutBack) => format!(
            "    mut taken{s} = {to}[\"{key}\"]\n    taken{s}.append({n})\n    {to}[\"{key}\"] = taken{s}\n"
        ),
        (Held::Lists, _) => format!("    {to}[\"{key}\"].append({n})\n"),
        (Held::Spots, Writing::Field) => format!("    {to}[\"{key}\"].x += {n}\n"),
        (Held::Spots, Writing::PutBack) => format!(
            "    mut taken{s} = {to}[\"{key}\"]\n    taken{s}.y = {n}\n    {to}[\"{key}\"] = taken{s}\n"
        ),
        (Held::Spots, _) => format!("    {to}[\"{key}\"].moved({n})\n"),
        (Held::Counts, Writing::Set) => format!("    {to}[\"{key}\"] = {n}\n"),
        (Held::Counts, _) => format!("    {to}[\"{key}\"] += {n}\n"),
        (Held::Rooms, Writing::Set) => format!("    {to}[{room}].counts[\"b\"] = {n}\n"),
        (Held::Rooms, Writing::Field) => format!("    {to}[{room}].label += \"q{n}\"\n"),
        (Held::Rooms, Writing::Method) => format!("    {to}[{room}].labeled(\"a{n}\", second=\"é{n}\")\n"),
        (Held::Rooms, Writing::PutBack) => format!(
            "    mut taken{s} = {to}[{room}]\n    taken{s}.counts[\"a\"] += {n}\n    {to}[{room}] = taken{s}\n"
        ),
        (Held::Rooms, _) => format!(
            "    for ref room in {to}:\n        room.counts[\"a\"] += {n}\n        room.label = room.label + \"p{n}\"\n"
        ),
        (Held::Ledger, Writing::Field) => format!("    {to}.count += {n}\n"),
        (Held::Table | Held::Ledger | Held::Entries, Writing::Set) => {
            format!("    {table}[{point}].x = {n}\n")
        }
        (Held::Table | Held::Ledger | Held::Entries, Writing::PutBack) => format!(
            "    mut taken{s} = {table}[{point}]\n    taken{s}.y += {n}\n    {table}[{point}] = taken{s}\n"
        ),
        (Held::Table | Held::Ledger | Held::Entries, Writing::Method) => {
            format!("    {table}[{point}].moved({n})\n")
        }
        (Held::Table | Held::Ledger | Held::Entries, _) => {
            format!("    for ref point in {table}:\n        point.x += {n}\n")
        }
        (Held::Point, Writing::Field) => format!("    {to}.x = {n}\n"),
        (Held::Point, _) => format!("    {to}.moved({n})\n"),
        (Held::Text, Writing::Added) => format!("    {to} += \"q{n}\"\n"),
        (Held::Text, Writing::Formatted) => format!("    {to} = f\"{{{to}}}r{n}\"\n"),
        (Held::Text, Writing::Chained) => format!("    {to} = {to} + \"a{n}\" + {to}\n"),
        (Held::Text, Writing::Summed) => format!("    {to} += \"b{n}\" + {to} + \"ç\"\n"),
        (Held::Text, Writing::Long) => format!("    {to} += \"{LONG_PIECE}{n}\"\n"),
        (Held::Text, _) => format!("    {to} = {to} + \"p{n}\"\n"),
    }
}

/// `constants{id}`, the function a `Constants` calls, after the constants, the struct whose
/// fields default to them, and the function using them that it runs on each thread.
fn constants_written(constants: &Constants) -> String {
    let Constants { id, base, reads, main_first, launch_first } = *constants;
    let mut out = format!(
        "NUMBERS{id} = [{base}, {}]\nCOUNTS{id} = {{\"a\": {base}}}\nNOTHING{id} = list[(str, int)]()\nSPOT{id} = \
         Point(x={base}, y=1)\n\nstruct Bin{id}:\n    at = 0\n    items = NOTHING{id}\n    counts = COUNTS{id}\n    \
         numbers = NUMBERS{id}\n\n    def pushed(ref self, n: int):\n        self.items.append((\"x\", n))\n        \
         self.counts[\"n\"] = n\n        self.numbers.append(n)\n\n    def top(ref self) -> int:\n        last = \
         self.items.pop() else return -1\n        return last[1]\n\n",
        base + 1
    );
    let _ = write!(
        out,
        "def used{id}(n: int) -> str:\n    mut bin = Bin{id}()\n    bin.pushed(n)\n    top = bin.top()\n    mut mine = \
         NUMBERS{id}\n    mine.append(n)\n    mine[0] = n\n    mut spot = SPOT{id}\n    spot.moved(n)\n    mut counts = \
         COUNTS{id}\n    counts[\"m\"] = n\n    return f\"{{top}} {{len(bin.items)}} {{bin.counts}} {{bin.numbers}} {{mine}} \
         {{spot}} {{counts}} {{len(NOTHING{id})}} {{NUMBERS{id}}} {{COUNTS{id}}} {{SPOT{id}}}\"\n\nstruct Said{id}:\n    \
         texts: list[str]\n\ndef tasked{id}(said: shared Said{id}):\n    text = used{id}(2)\n    lock said as ref held:\n        \
         held.texts.append(text)\n    return\n\n"
    );
    let _ = writeln!(out, "def constants{id}(stdout: Stdout) fails Crashed:");
    // The task hands back what it worked out rather than printing it, so what's checked is what
    // it saw, whatever order the threads' output comes in.
    let _ = writeln!(out, "    shared said = Said{id}(texts=list[str]())");
    for _ in 0..reads {
        let _ = writeln!(
            out,
            "    stdout.print(len(NUMBERS{id}), len(COUNTS{id}), len(NOTHING{id}), SPOT{id}.x)"
        );
    }
    if main_first {
        let _ = writeln!(out, "    stdout.print(\"main\", used{id}(1))");
    }
    let launched = format!(
        "    with launch(() => tasked{id}(said))?:\n        pass\n    lock said as held:\n        for text in held.texts:\n            \
         stdout.print(\"task\", text)\n"
    );
    let parallel = format!(
        "    for line in [used{id}(i) for i in parallel(range(3))]:\n        stdout.print(\"parallel\", line)\n"
    );
    if launch_first {
        out.push_str(&launched);
        out.push_str(&parallel);
    } else {
        out.push_str(&parallel);
        out.push_str(&launched);
    }
    let _ = writeln!(
        out,
        "    stdout.print(\"after\", used{id}(3), NUMBERS{id}, COUNTS{id}, len(NOTHING{id}))\n    return\n"
    );
    out
}

/// `fielded{id}`, the function a `FieldCalls` calls, after its struct and the functions its
/// calls go to: each `+=` made, the field printed after it, then the struct and its `shared`
/// one printed.
fn field_calls_written(calls: &FieldCalls) -> String {
    let FieldCalls { id, base, steps } = calls;
    let mut out = format!(
        "struct Box{id}:\n    text: str\n    count: int\n    items: list[int]\n\n\
         def told{id}(stdout: Stdout, n: int) -> int:\n    stdout.print(\"told\", n)\n    return n\n\n\
         def said{id}(stdout: Stdout, n: int) -> str:\n    stdout.print(\"said\", n)\n    return f\"s{{n}}\"\n\n\
         def given{id}(stdout: Stdout, n: int) -> list[int]:\n    stdout.print(\"given\", n)\n    return [n, n]\n\n\
         def capped_count{id}(n: int) -> int fails Refused:\n    if n > 5:\n        fail Refused(code=n)\n    return n\n\n\
         def capped_text{id}(n: int) -> str fails Refused:\n    if n > 5:\n        fail Refused(code=n)\n    return f\"c{{n}}\"\n\n\
         def capped_items{id}(n: int) -> list[int] fails Refused:\n    if n > 5:\n        fail Refused(code=n)\n    return [n]\n\n\
         def peeked_count{id}(stdout: Stdout, other: Box{id}) -> int:\n    stdout.print(\"peek\", other.count)\n    return other.count\n\n\
         def peeked_text{id}(stdout: Stdout, other: Box{id}) -> str:\n    stdout.print(\"peek\", other.text)\n    return other.text\n\n\
         def peeked_items{id}(stdout: Stdout, other: Box{id}) -> list[int]:\n    stdout.print(\"peek\", other.items)\n    return other.items\n\n"
    );
    for (s, step) in steps.iter().enumerate() {
        if step.call == Calling::Failed {
            let (field, _) = field_names(step.field);
            let _ = write!(
                out,
                "def failed{id}_{s}(ref box: Box{id}, n: int) fails Refused:\n    box.{field} += {}capped_{field}{id}(n)?\n    return\n\n",
                plus_written(step.field, step.plus, step.n)
            );
        }
    }
    let _ = write!(
        out,
        "def fielded{id}(stdout: Stdout):\n    mut box = Box{id}(text=\"t{base}\", count={base}, items=[{base}])\n    \
         shared kept = Box{id}(text=\"k\", count=0, items=list[int]())\n"
    );
    for (s, step) in steps.iter().enumerate() {
        let FieldCall { field, call, plus, n } = *step;
        let (name, _) = field_names(field);
        let plus = plus_written(field, plus, n);
        let maker = match field {
            Field::Count => "told",
            Field::Text => "said",
            Field::Items => "given",
        };
        match call {
            Calling::Printed => {
                let _ = writeln!(out, "    box.{name} += {plus}{maker}{id}(stdout, {n})");
            }
            Calling::Failed => {
                let _ = writeln!(
                    out,
                    "    failed{id}_{s}(ref box, n={n}) except Refused():\n        stdout.print(\"refused\")"
                );
            }
            Calling::Copied => {
                let _ = writeln!(
                    out,
                    "    snap{s} = box\n    box.{name} += {plus}peeked_{name}{id}(stdout, snap{s})"
                );
            }
            Calling::Listed => {
                let _ = writeln!(
                    out,
                    "    held{s} = [box]\n    box.{name} += {plus}peeked_{name}{id}(stdout, held{s}[0])"
                );
            }
            Calling::Closure => {
                let _ = writeln!(out, "    look{s} = () => box.{name}\n    box.{name} += {plus}look{s}()");
            }
            Calling::Shared => {
                let _ = writeln!(
                    out,
                    "    lock kept as ref held:\n        held.{name} += {plus}{maker}{id}(stdout, {n})"
                );
            }
        }
        if call != Calling::Shared {
            let _ = writeln!(out, "    stdout.print(\"now\", box.{name})");
        }
    }
    out.push_str("    stdout.print(\"box\", box)\n    lock kept as held:\n        stdout.print(\"kept\", held)\n    return\n\n");
    out
}

/// A field's name, and the kind its calls make.
const fn field_names(field: Field) -> (&'static str, &'static str) {
    match field {
        Field::Text => ("text", "str"),
        Field::Count => ("count", "int"),
        Field::Items => ("items", "list[int]"),
    }
}

/// The `a + ` before the call on a `+=`'s right side, when it has one.
fn plus_written(field: Field, plus: bool, n: i64) -> String {
    if !plus {
        return String::new();
    }
    match field {
        Field::Text => format!("\"a{n}\" + "),
        Field::Count => format!("{n} + "),
        Field::Items => format!("[{n}] + "),
    }
}

/// `ordered{id}`, the function an `Ordered` calls, after its structs and `three{id}`: a line for
/// each step, then what `tally` and `pair` hold at the end.
fn ordered_written(in_order: &InOrder) -> String {
    let InOrder { id, base, steps } = in_order;
    let mut out = format!(
        "struct Tally{id}:\n    items: list[int]\n    count: int\n    names: dict[str, int]\n\n    def grow(ref \
         self, by: int) -> int:\n        self.items.append(self.count + by)\n        self.count += 1\n        return \
         self.count\n\n    def key(ref self, by: int) -> str:\n        self.names[\"a\"] = self.names[\"a\"] + \
         by\n        return \"a\"\n\n    def show(self, given: int) -> str:\n        return f\"{{self.items}} \
         {{self.count}} {{given}}\"\n\nstruct Pair{id}:\n    left: Tally{id}\n    right: int\n\n    def \
         shown(self, given: int) -> str:\n        return f\"{{self.left.count}} {{self.right}} {{given}}\"\n\ndef \
         three{id}(first: int, second: int, third: int) -> str:\n    return f\"{{first}} {{second}} \
         {{third}}\"\n\ndef ordered{id}(stdout: Stdout):\n    mut tally = Tally{id}(items=[{base}, {}, {}, {}], \
         count={base}, names={{\"a\": {base}}})\n    mut pair = Pair{id}(left=tally, right={base})\n",
        base + 1,
        base + 2,
        base + 3
    );
    for (step, n) in steps {
        let grow = format!("tally.grow({n})");
        let line = match step {
            OrderStep::Receiver => format!("stdout.print(\"receiver\", tally.show({grow}))"),
            OrderStep::FieldReceiver => {
                format!("stdout.print(\"field receiver\", pair.shown(pair.left.grow({n})))")
            }
            OrderStep::Counted => format!("stdout.print(\"counted\", tally.items.count({grow}))"),
            OrderStep::Got => format!("stdout.print(\"got\", tally.names.get(tally.key({n})))"),
            OrderStep::Indexed => format!("stdout.print(\"indexed\", tally.items[{grow} % 4])"),
            OrderStep::Keyed => format!("stdout.print(\"keyed\", tally.names[tally.key({n})])"),
            OrderStep::Arguments => format!(
                "stdout.print(\"arguments\", three{id}(tally.count, second={grow}, third=tally.count))"
            ),
            OrderStep::PlusBefore => format!("stdout.print(\"plus before\", tally.count + {grow})"),
            OrderStep::PlusAfter => format!("stdout.print(\"plus after\", {grow} + tally.count)"),
            OrderStep::Listed => format!("stdout.print(\"listed\", [tally.count, {grow}, tally.count])"),
            OrderStep::Text => {
                format!("stdout.print(\"text\", f\"{{tally.count}} {{{grow}}} {{tally.count}}\")")
            }
            OrderStep::AtLeast => format!("stdout.print(\"at least\", tally.count >= {grow})"),
            OrderStep::FieldWrite => {
                format!("tally.count = {grow} + {n}\n    stdout.print(\"field write\", tally.count)")
            }
            OrderStep::IndexWrite => {
                format!("tally.items[{grow} % 4] = {grow}\n    stdout.print(\"index write\", tally.items)")
            }
            OrderStep::Added => format!("tally.count += {grow}\n    stdout.print(\"added\", tally.count)"),
        };
        let _ = writeln!(out, "    {line}");
    }
    let _ = writeln!(
        out,
        "    stdout.print(\"tally\", tally.items, tally.count, tally.names)\n    stdout.print(\"pair\", \
         pair.left.items, pair.left.count, pair.right)\n    return\n"
    );
    out
}

/// `text` as a Fennec string literal.
fn text_literal(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The functions a `Filed` statement calls, `{N}` standing for its id: its failure, the writes
/// that can stop partway, what a file holds, and a file cleared if it's there.
const FILED_TYPES: &str = "error Halt{N}:
    at: int

def saved{N}(fs: Fs, path: str, pieces: list[str], stop_at: int) fails IoError | Halt{N}:
    with fs.create(path)? as file:
        mut at = 0
        for piece in pieces:
            if at == stop_at:
                fail Halt{N}(at=at)
            file.write_text(piece)?
            at += 1

def halted{N}(at: int, stop_at: int) fails Halt{N}:
    if at == stop_at:
        fail Halt{N}(at=at)

def questioned{N}(fs: Fs, path: str, pieces: list[str], stop_at: int) fails IoError | Halt{N}:
    with fs.create(path, durable=False)? as file:
        mut at = 0
        for piece in pieces:
            file.write_text(piece)?
            file.sync()?
            halted{N}(at, stop_at=stop_at)?
            at += 1

def both{N}(fs: Fs, first: str, second: str, pieces: list[str], stop_at: int) fails IoError | Halt{N}:
    with fs.create(first)? as one, fs.create(second)? as two:
        mut at = 0
        for piece in pieces:
            one.write_text(piece)?
            if at == stop_at:
                fail Halt{N}(at=at)
            two.write_text(piece)?
            at += 1

def shown{N}(fs: Fs, path: str) -> str:
    found = fs.read_text_if_present(path) except why:
        return f\"unreadable {why}\"
    text = found else return \"nothing\"
    return f\"[{text}]\"

def cleared{N}(fs: Fs, path: str) fails IoError:
    if fs.exists(path)?:
        fs.remove(path)?

";

/// The task a `Filed` statement's `Tasked` steps launch, which crashes partway through its write,
/// and the function that launches it. Only a statement with such a step has them, since a program
/// without threads can't launch.
const FILED_TASKS: &str = "def written{N}(fs: Fs, path: str, pieces: list[str], stop_at: int) fails IoError:
    with fs.create(path)? as file:
        mut at = 0
        for piece in pieces:
            file.write_text(piece)?
            if at == stop_at:
                _ = pieces[at + 99]
            at += 1

def tasked{N}(fs: Fs, path: str, pieces: list[str], stop_at: int) fails Crashed | IoError:
    with launch(() => written{N}(fs, path, pieces, stop_at=stop_at)?) as task:
        pass

";

/// `filed{id}`, the function a `Filed` calls, after the functions it calls: its folder made if
/// it's not there, both files cleared, then each step and what the file holds after it.
fn filed_written(filing: &Filing) -> String {
    let Filing { id, steps } = filing;
    let mut out = FILED_TYPES.replace("{N}", &id.to_string());
    if steps.iter().any(|step| step.way == FileWay::Tasked) {
        out.push_str(&FILED_TASKS.replace("{N}", &id.to_string()));
    }
    let failed = |indent: &str| format!("except why:\n{indent}    stdout.print(\"failed\", why)");
    let (first, second) = (format!("\"d{id}/f.txt\""), format!("\"d{id}/g.txt\""));
    let _ = write!(
        out,
        "def filed{id}(fs: Fs, stdout: Stdout):\n    present = fs.exists(\"d{id}\") {}\n        return\n    if not \
         present:\n        fs.make_dir(\"d{id}\") {}\n            return\n    cleared{id}(fs, {first}) {}\n        \
         return\n    cleared{id}(fs, {second}) {}\n        return\n",
        failed("    "),
        failed("        "),
        failed("    "),
        failed("    "),
    );
    for step in steps {
        let path = if step.second { &second } else { &first };
        let pieces: Vec<String> =
            step.pieces.iter().map(|at| text_literal(FILE_PIECES.get(*at).copied().unwrap_or(""))).collect();
        let whole: String =
            step.pieces.iter().map(|at| FILE_PIECES.get(*at).copied().unwrap_or("")).collect();
        let (pieces, stop_at, whole) = (pieces.join(", "), step.stop_at, text_literal(&whole));
        let shown = format!("stdout.print(shown{id}(fs, {path}))");
        let lines = match step.way {
            FileWay::Saved | FileWay::Questioned => {
                let called = if step.way == FileWay::Saved { "saved" } else { "questioned" };
                format!(
                    "{called}{id}(fs, {path}, pieces=[{pieces}], stop_at={stop_at}) {}\n    {shown}",
                    failed("    ")
                )
            }
            FileWay::Tasked => format!(
                "tasked{id}(fs, {path}, pieces=[{pieces}], stop_at={stop_at}) except why:\n        case \
                 Crashed():\n            stdout.print(\"crashed\")\n        case _:\n            \
                 stdout.print(\"failed\", why)\n    {shown}"
            ),
            FileWay::Both => format!(
                "both{id}(fs, first={first}, second={second}, pieces=[{pieces}], stop_at={stop_at}) {}\n    \
                 stdout.print(shown{id}(fs, {first}), shown{id}(fs, {second}))",
                failed("    ")
            ),
            FileWay::Plain => format!("fs.write_text({path}, {whole}) {}\n    {shown}", failed("    ")),
            FileWay::ReadOnly => format!(
                "fs.read_only().write_text({path}, {whole}) except why:\n        stdout.print(\"refused\", \
                 why)\n    {shown}"
            ),
            FileWay::Outside => format!(
                "fs.only(\"d{id}\").write_text(\"elsewhere.txt\", {whole}) except why:\n        case \
                 .PermissionDenied():\n            stdout.print(\"refused outside\")\n        case _:\n            \
                 stdout.print(\"failed\", why)"
            ),
            FileWay::Removed => format!(
                "fs.remove({path}) except why:\n        case .NotFound():\n            stdout.print(\"not \
                 there\")\n        case _:\n            stdout.print(\"failed\", why)\n    {shown}"
            ),
        };
        let _ = writeln!(out, "    {lines}");
    }
    out.push('\n');
    out
}

/// The types an `Owned` statement changes through their own `def []`, `{N}` standing for its id:
/// a failing `Part`, a `Shelf`, `Keyed`, `Room` and `Rows` with the plain form and a `def []=`
/// that counts its writes, and `Slots` with the `ref` form, `len` and `is_live`.
const OWNED_TYPES: &str = "error Nope{N}:
    code: int

struct Part{N}:
    name: str
    x = 0
    marks = list[int]()

    def moved(ref self, by: int):
        self.x += by
        self.marks.append(by)
        self.x += by

    def shift(ref self, by: int) fails Nope{N}:
        self.x += by
        self.marks.append(by)
        if by > 5:
            fail Nope{N}(code=by)
        self.x += 100

struct Shelf{N}:
    parts: list[Part{N}]
    writes = 0

    def [](self, at: int) -> Part{N}:
        return self.parts[at]

    def []=(ref self, at: int, part: Part{N}):
        self.parts[at] = part
        self.writes += 1

struct Keyed{N}:
    parts: dict[str, Part{N}]
    writes = 0

    def [](self, key: str) -> Part{N}:
        return self.parts[key]

    def []=(ref self, key: str, part: Part{N}):
        self.parts[key] = part
        self.writes += 1

struct Room{N}:
    shelf: Shelf{N}
    writes = 0

    def [](self, name: str) -> Shelf{N}:
        return self.shelf

    def []=(ref self, name: str, shelf: Shelf{N}):
        self.shelf = shelf
        self.writes += 1

struct Rows{N}:
    rows: list[list[int]]
    writes = 0

    def [](self, at: int) -> list[int]:
        return self.rows[at]

    def []=(ref self, at: int, row: list[int]):
        self.rows[at] = row
        self.writes += 1

struct Slots{N}:
    items: list[int]
    alive: list[bool]

    def len(self) -> int:
        return len(self.items)

    def is_live(self, at: int) -> bool:
        return self.alive[at]

    def [](ref self, at: int) -> int:
        return self.items[at]

def bumped{N}(ref value: int, by: int):
    value += by

def passed{N}(ref shelf: Shelf{N}, at: int, by: int) fails Nope{N}:
    shelf[at].shift(by)?
    shelf[at].x += 1000

";

/// `owned{id}`, the function an `Owned` calls, after its types: the values, a line for each step,
/// then what each holds and how often its `def []=` ran.
fn owned_written(owning: &OwnIndexing) -> String {
    let OwnIndexing { id, base, steps } = owning;
    let mut out = OWNED_TYPES.replace("{N}", &id.to_string());
    let (b1, b2, b3, b4, b5) = (base + 1, base + 2, base + 3, base + 4, base + 5);
    let _ = write!(
        out,
        "def owned{id}(stdout: Stdout):\n    mut shelf = Shelf{id}(parts=[Part{id}(name=\"a\", x={base}), \
         Part{id}(name=\"b\", x={b1})])\n    mut keyed = Keyed{id}(parts={{\"k\": Part{id}(name=\"k\", \
         x={base})}})\n    mut room = Room{id}(shelf=Shelf{id}(parts=[Part{id}(name=\"c\", x={base}), \
         Part{id}(name=\"d\", x={b1})]))\n    mut rows = Rows{id}(rows=[[{b2}, {base}, {b2}], [{b4}, {b3}, \
         {b5}]])\n    mut slots = Slots{id}(items=[{base}, {b1}, {b2}], alive=[True, False, True])\n"
    );
    for &(step, at, n) in steps {
        let pair = at % 2;
        let other = 1 - pair;
        let failed = "except why:\n        stdout.print(\"failed\", why.code)";
        let line = match step {
            OwnStep::Plus => format!("shelf[{pair}].x += {n}"),
            OwnStep::Set => format!("shelf[{pair}].x = {n}"),
            OwnStep::Moved => format!("shelf[{pair}].moved({n})"),
            OwnStep::Appended => format!("shelf[{pair}].marks.append({n})"),
            OwnStep::Shifted => format!("shelf[{pair}].shift({n}) {failed}"),
            OwnStep::Passed => format!("passed{id}(ref shelf, at={pair}, by={n}) {failed}"),
            OwnStep::RefField => format!("bumped{id}(ref shelf[{pair}].x, by={n})"),
            OwnStep::PlusOther => format!("shelf[{pair}].x += shelf[{other}].x"),
            OwnStep::Keyed => format!("keyed[\"k\"].x += {n}"),
            OwnStep::KeyedMoved => format!("keyed[\"k\"].moved({n})"),
            OwnStep::Nested => format!("room[\"back\"][{pair}].x += {n}"),
            OwnStep::NestedMoved => format!("room[\"back\"][{pair}].moved({n})"),
            OwnStep::NestedRef => format!("bumped{id}(ref room[\"back\"][{pair}].x, by={n})"),
            OwnStep::Counted => {
                format!(
                    "stdout.print(\"counted\", rows[{pair}].count({n}), rows[{pair}].get({n}), rows.writes)"
                )
            }
            OwnStep::Sorted => format!("rows[{pair}].sort()"),
            OwnStep::SortedKey => format!("rows[{pair}].sort(key=item => 0 - item)"),
            OwnStep::Removed => format!("rows[{pair}].remove_where(item => item > {n})"),
            OwnStep::SlotSet => format!("slots[{at}] = {n}"),
            OwnStep::SlotPlus => format!("slots[{at}] += {n}"),
            OwnStep::SlotRef => format!("bumped{id}(ref slots[{at}], by={n})"),
            OwnStep::ForRef => format!("for ref slot in slots:\n        slot = slot * 2 + {n}"),
            OwnStep::ParallelRef => format!("for ref slot in parallel(slots):\n        slot += {n}"),
        };
        let _ = writeln!(out, "    {line}");
    }
    let _ = writeln!(
        out,
        "    stdout.print(\"shelf\", shelf.parts, shelf.writes)\n    stdout.print(\"keyed\", keyed.parts, \
         keyed.writes)\n    stdout.print(\"room\", room.shelf.parts, room.shelf.writes, room.writes)\n    \
         stdout.print(\"rows\", rows.rows, rows.writes)\n    stdout.print(\"slots\", slots.items)\n"
    );
    out
}

/// The generic holder and functions a `Generic` statement calls, `{N}` standing for its id.
const GENERIC_FUNCTIONS: &str = "struct Holder{N}[T]:
    item: T
    items: list[T]

    def put(ref self, value: T):
        self.items.append(self.item)
        self.item = value

    def oldest(self) -> T:
        if len(self.items) > 0:
            return self.items[0]
        return self.item

def swapped{N}[T](ref one: T, ref other: T):
    held = one
    one = other
    other = held

def grown{N}[T](ref items: list[T], value: T, times: int):
    for _ in range(times):
        items.append(value)

def doubled{N}[T](value: T) -> list[T]:
    mut out = [value]
    out.append(value)
    return out

def changed_copy{N}[T](holder: Holder{N}[T], value: T) -> Holder{N}[T]:
    mut mine = holder
    mine.put(value)
    return mine

";

/// `generic{id}`, the function a `Generic` calls, after its holder and functions: the values, a
/// line for each step, then what each holds.
fn generic_written(generic: &Genericking) -> String {
    let Genericking { id, ty, steps } = generic;
    let mut out = String::new();
    if *ty == GenericTy::Point {
        let _ = write!(out, "struct Point{id}:\n    x: int\n    tags: list[str]\n\n");
    }
    out.push_str(&GENERIC_FUNCTIONS.replace("{N}", &id.to_string()));
    let values = ty.values(*id);
    let value = |at: usize| values.get(at).map_or("", |(written, _)| written.as_str()).to_owned();
    let (t, v0, v1, v2) = (ty.written(*id), value(0), value(1), value(2));
    let _ = write!(
        out,
        "def generic{id}(stdout: Stdout):\n    mut holder = Holder{id}[{t}](item={v0}, items=[])\n    mut other = \
         Holder{id}[{t}](item={v1}, items=[{v2}])\n    mut outer = Holder{id}[Holder{id}[{t}]](item=holder, \
         items=[])\n    mut pair = [holder, other]\n    mut table = {{\"k\": other}}\n"
    );
    for (at, &(step, which, count)) in steps.iter().enumerate() {
        let v = value(which);
        let line = match step {
            GenericStep::Put => format!("holder.put({v})"),
            GenericStep::Grown => format!("grown{id}(ref holder.items, value={v}, times={count})"),
            GenericStep::Swapped => format!("swapped{id}(ref holder.item, ref other.item)"),
            GenericStep::SwappedItems => format!("swapped{id}(ref holder.items, ref other.items)"),
            GenericStep::Copied => format!("stdout.print(changed_copy{id}(holder, value={v}))"),
            GenericStep::Doubled => format!("stdout.print(doubled{id}(holder.item))"),
            GenericStep::Oldest => "stdout.print([holder.oldest()])".to_owned(),
            GenericStep::Nested => format!("outer.item.put({v})"),
            GenericStep::ForRef => format!("for ref each in pair:\n        each.put({v})"),
            GenericStep::Popped => {
                format!("taken{at} = holder.items.pop() else holder.item\n    stdout.print([taken{at}])")
            }
            GenericStep::DictPut => format!("table[\"k\"].put({v})"),
        };
        let _ = writeln!(out, "    {line}");
    }
    out.push_str(
        "    stdout.print(holder)\n    stdout.print(other)\n    stdout.print(outer)\n    stdout.print(pair)\n    \
         stdout.print(table)\n\n",
    );
    out
}

/// `stuck{id}`, the function a `Stuck` calls, and the two it may: `drained{id}`, a task receiving
/// from the channel, and `taken{id}`, a round doing the same.
fn stuck_written(sticking: Sticking) -> String {
    let Sticking { id, way, n } = sticking;
    let waiting = match way {
        StuckWay::TimedReceive => format!(
            "got = jobs.receive_within({n}ms) except Timeout(): -1\n        stdout.print(\"received\", got else -2)"
        ),
        StuckWay::TimedSend => format!(
            "jobs.send(1)?\n        jobs.send_within(2, {n}ms) except why:\n            case Timeout():\n                \
             stdout.print(\"send timed out\")\n            case _:\n                stdout.print(\"failed\", why)"
        ),
        StuckWay::Received => {
            "number = jobs.receive() else return\n        stdout.print(\"never\", number)".to_owned()
        }
        StuckWay::SentFull => {
            "jobs.send(1)?\n        jobs.send(2)?\n        stdout.print(\"never\")".to_owned()
        }
        StuckWay::BlockEnd => {
            format!("with launch(() => drained{id}(jobs, stdout)) as task:\n            pass")
        }
        StuckWay::Rounds => {
            format!("stdout.print([taken{id}(number, jobs=jobs) for number in parallel(range({n}))])")
        }
    };
    format!(
        "def drained{id}(jobs: Channel[int], stdout: Stdout):\n    stdout.print(\"drained{id} waits\")\n    number = \
         jobs.receive() else return\n    stdout.print(\"never\", number)\n\ndef taken{id}(number: int, jobs: \
         Channel[int]) -> int:\n    return (jobs.receive() else -1) + number\n\ndef stuck{id}(stdout: Stdout) fails \
         Crashed | Closed | Timeout:\n    with channel[int](size=1) as jobs:\n        stdout.print(\"stuck{id}\", \
         {n})\n        {waiting}\n\n"
    )
}

/// The interface, its two types and the generic functions an `Interfaced` statement calls, `{N}`
/// standing for its id.
const INTERFACED_TYPES: &str = "error Nope{N}:
    code: int

interface Grower{N}:
    def grow(ref self, by: int) -> int
    def size(self) -> int
    def tried(ref self, by: int) -> int fails Nope{N}
    def counts(self) -> Iterator[int]

struct Crate{N}:
    items: list[int]

    def grow(ref self, by: int) -> int:
        self.items.append(len(self.items) + by)
        return len(self.items)

    def size(self) -> int:
        return len(self.items)

    def tried(ref self, by: int) -> int fails Nope{N}:
        self.items.append(by)
        if by > 5:
            fail Nope{N}(code=by)
        return len(self.items)

    def counts(self) -> Iterator[int]:
        for item in self.items:
            yield item

struct Heap{N}:
    held: int

    def grow(ref self, by: int) -> int:
        self.held += by + 3
        return self.held

    def size(self) -> int:
        return self.held

    def tried(ref self, by: int) -> int fails Nope{N}:
        self.held += by
        if by > 5:
            fail Nope{N}(code=by)
        return self.held

    def counts(self) -> Iterator[int]:
        for at in range(self.held % 4):
            yield at

def grown_all{N}[G: Grower{N}](ref items: list[G], by: int) -> int:
    mut total = 0
    for ref item in items:
        total += item.grow(by)
    return total

def bumped{N}[G: Grower{N}](ref item: G, by: int):
    _ = item.grow(by)
    _ = item.grow(by + 1)

def copied{N}[G: Grower{N}](item: G, by: int) -> G:
    mut mine = item
    _ = mine.grow(by)
    return mine

def summed{N}[G: Grower{N}](item: G) -> int:
    return sum(item.counts())

def tried_twice{N}[G: Grower{N}](ref item: G, by: int) -> int fails Nope{N}:
    first = item.tried(by)?
    second = item.tried(by + 4)?
    return first + second

def sized{N}[G: Grower{N}](items: list[G]) -> int:
    mut total = 0
    for item in items:
        total += item.size()
    return total

";

/// `interfaced{id}`, the function an `Interfaced` calls, after its types and functions: a list
/// of each type and one of each, a line for each step, then what each holds.
fn interfaced_written(interfacing: &Interfacing) -> String {
    let Interfacing { id, steps } = interfacing;
    let mut out = INTERFACED_TYPES.replace("{N}", &id.to_string());
    let _ = write!(
        out,
        "def interfaced{id}(stdout: Stdout):\n    mut crates = [Crate{id}(items=[1]), Crate{id}(items=[])]\n    mut \
         heaps = [Heap{id}(held=2), Heap{id}(held=5)]\n    mut crate = Crate{id}(items=[5])\n    mut heap = \
         Heap{id}(held=1)\n"
    );
    for (at, &(step, crate_first, n)) in steps.iter().enumerate() {
        let (items, item) = if crate_first { ("crates", "crate") } else { ("heaps", "heap") };
        let line = match step {
            IfaceStep::GrownAll => format!("stdout.print(grown_all{id}(ref {items}, by={n}))"),
            IfaceStep::Bumped => format!("bumped{id}(ref {item}, by={n})"),
            IfaceStep::Copied => format!("stdout.print(copied{id}({item}, by={n}), {item})"),
            IfaceStep::Summed => format!("stdout.print(summed{id}({item}))"),
            IfaceStep::Tried => format!(
                "got{at} = tried_twice{id}(ref {item}, by={n}) except why:\n        stdout.print(\"failed\", \
                 why)\n        -1\n    stdout.print(got{at})"
            ),
            IfaceStep::EachBumped => {
                format!("for ref each in {items}:\n        bumped{id}(ref each, by={n})")
            }
            IfaceStep::Sized => format!("stdout.print(sized{id}({items}))"),
        };
        let _ = writeln!(out, "    {line}");
    }
    out.push_str(
        "    stdout.print(crates)\n    stdout.print(heaps)\n    stdout.print(crate)\n    stdout.print(heap)\n\n",
    );
    out
}

/// The accounts and functions a `LockedTogether` statement calls, `{N}` standing for its id. Each
/// that locks two has a twin, `..._against`, naming them the other way round.
fn locked_together_types(id: usize) -> String {
    let mut out = format!(
        "error Short{id}:\n    left: int\n\nstruct Account{id}:\n    total: int\n    moves: list[int]\n\ndef \
         shown{id}(account: shared Account{id}) -> str:\n    lock account as held:\n        return \
         f\"{{held.total}}{{held.moves}}\"\n\n"
    );
    for (suffix, lock) in
        [("", "first, second as ref one, ref two"), ("_against", "second, first as ref two, ref one")]
    {
        let read_lock =
            if suffix.is_empty() { "first, second as one, two" } else { "second, first as two, one" };
        let half_lock =
            if suffix.is_empty() { "first, second as ref one, two" } else { "second, first as two, ref one" };
        let _ = write!(
            out,
            "def moved{id}{suffix}(first: shared Account{id}, second: shared Account{id}, amount: int, backward: bool) \
             fails Short{id}:\n    lock {lock}:\n        if backward:\n            two.total -= amount\n            \
             two.moves.append(-amount)\n            one.total += amount\n            one.moves.append(amount)\n            \
             if two.total < 0:\n                fail Short{id}(left=two.total)\n        else:\n            one.total \
             -= amount\n            one.moves.append(-amount)\n            two.total += amount\n            \
             two.moves.append(amount)\n            if one.total < 0:\n                fail Short{id}(left=one.total)\n\n\
             def crashed{id}{suffix}(first: shared Account{id}, second: shared Account{id}, amount: int):\n    lock \
             {lock}:\n        one.total += amount\n        two.moves.append(amount)\n        _ = one.moves[amount + \
             99]\n\ndef summed{id}{suffix}(first: shared Account{id}, second: shared Account{id}) -> int:\n    lock \
             {read_lock}:\n        return one.total + two.total + len(one.moves) + len(two.moves)\n\ndef \
             half_ref{id}{suffix}(first: shared Account{id}, second: shared Account{id}, amount: int):\n    lock \
             {half_lock}:\n        one.total += two.total % 7 + amount\n        one.moves.append(len(two.moves))\n\n"
        );
    }
    out
}

/// `locked_together{id}`, the function a `LockedTogether` calls, after its accounts and functions:
/// the accounts, a line for each step and what all three hold after it.
fn locked_together_written(locking: &LockingTogether) -> String {
    let LockingTogether { id, base, steps } = locking;
    let mut out = locked_together_types(*id);
    let threads = steps.iter().any(|step| step.way == LockWay::Crashed);
    if threads {
        for suffix in ["", "_against"] {
            let _ = write!(
                out,
                "def tasked{id}{suffix}(first: shared Account{id}, second: shared Account{id}, amount: int) fails \
                 Crashed:\n    with launch(() => crashed{id}{suffix}(first=first, second=second, amount=amount)) as \
                 task:\n        pass\n\n"
            );
        }
    }
    let _ = write!(
        out,
        "def locked_together{id}(stdout: Stdout):\n    shared a = Account{id}(total={base}, moves=[])\n    shared b = \
         Account{id}(total={}, moves=[1])\n    shared c = Account{id}(total={}, moves=[2, 3])\n",
        base + 1,
        base + 2
    );
    let names = ["a", "b", "c"];
    for step in steps {
        let name = |at: usize| names.get(at % 3).copied().unwrap_or("a");
        let (first, second) = (name(step.pair.0), name(step.pair.1));
        let suffix = if step.against { "_against" } else { "" };
        let (amount, backward) = (step.amount, if step.backward { "True" } else { "False" });
        let line = match step.way {
            LockWay::Moved => format!(
                "moved{id}{suffix}(first={first}, second={second}, amount={amount}, backward={backward}) except \
                 why:\n        stdout.print(\"failed\", why)"
            ),
            LockWay::Crashed => format!(
                "tasked{id}{suffix}(first={first}, second={second}, amount={amount}) except Crashed():\n        \
                 stdout.print(\"crashed\")"
            ),
            LockWay::Summed => format!("stdout.print(summed{id}{suffix}(first={first}, second={second}))"),
            LockWay::HalfRef => {
                format!("half_ref{id}{suffix}(first={first}, second={second}, amount={amount})")
            }
        };
        let _ = writeln!(out, "    {line}\n    stdout.print(shown{id}(a), shown{id}(b), shown{id}(c))");
    }
    out.push('\n');
    out
}

/// The struct and functions a `Ruled` statement uses, `{N}` standing for its id.
const RULE_TYPES: &str = "struct Gauge{N}:
    level: int where level >= 0
    tags = list[str]() where len(tags) <= 3
    name = \"g\" where len(name) > 0
    counts = [1, 2] where sum(counts) < 20

    def drained(ref self, by: int):
        self.level -= by

def picked{N}(n: int) -> int:
    return n

def lowered{N}(ref level: int, by: int):
    level -= by

def dipped{N}(ref level: int, by: int):
    level -= by
    level += by

def shrunk{N}(ref gauge: Gauge{N}, by: int):
    gauge.level -= by

def tagged{N}(ref tags: list[str], tag: str):
    tags.append(tag)

def flooded{N}(ref tags: list[str], stdout: Stdout):
    tags.extend([\"a\", \"b\", \"c\", \"d\"])
    stdout.print(\"flooded\", len(tags))

def made{N}(n: int, stdout: Stdout):
    got = Gauge{N}(level=picked{N}(n))
    stdout.print(\"made\", got)

";

/// `ruled{id}`, the function a `Ruled` calls, after its struct and functions: a gauge, two in a
/// list and one in a dict, then a line for each step, then what each holds.
fn ruled_written(ruling: &Ruling) -> String {
    let id = ruling.id;
    let mut out = RULE_TYPES.replace("{N}", &id.to_string());
    let _ = write!(
        out,
        "def ruled{id}(stdout: Stdout):\n    mut gauge = Gauge{id}(level=5)\n    mut gauges = [Gauge{id}(level=3), \
         Gauge{id}(level=8)]\n    mut by_name = {{\"k\": Gauge{id}(level=4)}}\n"
    );
    for (n, step) in ruling.steps.iter().enumerate() {
        let RuleStep { way, value: v, breaks } = *step;
        let at = v % 2;
        // What's taken from a level: no more than it holds, or one more when it breaks.
        let amount = |level: &str| {
            if breaks {
                format!("amount{n} = {level} + {}", v + 1)
            } else {
                format!("amount{n} = min({level}, {v})")
            }
        };
        let picked = if breaks { -(v + 1) } else { v };
        let line = match way {
            RuleWay::Set => format!("gauge.level = picked{id}({picked})"),
            RuleWay::Raised => format!("gauge.level += {v}"),
            RuleWay::Taken => format!("{}\n    gauge.level -= amount{n}", amount("gauge.level")),
            RuleWay::Lowered => format!("{}\n    lowered{id}(ref gauge.level, by=amount{n})", amount("gauge.level")),
            RuleWay::Dipped => {
                format!("amount{n} = gauge.level + {}\n    dipped{id}(ref gauge.level, by=amount{n})", v + 1)
            }
            RuleWay::Drained => format!("{}\n    gauge.drained(amount{n})", amount("gauge.level")),
            RuleWay::Shrunk => format!("{}\n    shrunk{id}(ref gauge, by=amount{n})", amount("gauge.level")),
            RuleWay::InList => {
                format!("{}\n    gauges[{at}].level -= amount{n}", amount(&format!("gauges[{at}].level")))
            }
            RuleWay::ForRef if breaks => {
                format!("for ref each in gauges:\n        each.level -= each.level + {}", v + 1)
            }
            RuleWay::ForRef => format!("for ref each in gauges:\n        each.level += {v}"),
            RuleWay::Counted if breaks => {
                "for ref each in gauge.counts:\n        each += 20\n        stdout.print(\"counted\", each)".to_owned()
            }
            RuleWay::Counted => {
                "for ref each in gauge.counts:\n        each += 50\n        each -= 50\n        stdout.print(\"counted\", \
                 each)"
                    .to_owned()
            }
            RuleWay::Tagged if breaks => "gauge.tags.extend([\"a\", \"b\", \"c\", \"d\"])".to_owned(),
            RuleWay::Tagged => format!("if len(gauge.tags) < 3:\n        gauge.tags.append(\"t{v}\")"),
            RuleWay::TaggedByRef if breaks => format!("flooded{id}(ref gauge.tags, stdout)"),
            RuleWay::TaggedByRef => {
                format!("if len(gauge.tags) < 3:\n        tagged{id}(ref gauge.tags, tag=\"r{v}\")")
            }
            RuleWay::Named if breaks => "gauge.name = gauge.name[0:0]".to_owned(),
            RuleWay::Named => format!("gauge.name = \"n{v}\""),
            RuleWay::Copied => format!("stdout.print(copy(gauge, level=picked{id}({picked})))"),
            RuleWay::Made => format!("made{id}({picked}, stdout)"),
            RuleWay::InDict => {
                format!("{}\n    by_name[\"k\"].level -= amount{n}", amount("by_name[\"k\"].level"))
            }
        };
        let _ = writeln!(out, "    {line}");
    }
    out.push_str("    stdout.print(gauge)\n    stdout.print(gauges)\n    stdout.print(by_name)\n\n");
    out
}

/// `popped{id}`, the function a `Popped` calls, after the types and the function it uses: a list
/// of optionals given three items, a step at a time on it, then what the list and the struct
/// holding a list of them hold.
fn popped_written(popping: &Popping) -> String {
    let Popping { id, ty, in_field, steps } = popping;
    let mut out = String::new();
    match ty {
        PopTy::Point => {
            let _ = write!(out, "struct Pt{id}:\n    x: int\n\n");
        }
        PopTy::Bag => {
            let _ = write!(out, "struct Bag{id}:\n    tag = \"b\"\n    items = list[int]()\n\n");
        }
        PopTy::Short | PopTy::Long | PopTy::Ints => {}
    }
    let t = ty.written(*id);
    let values = ty.values(*id);
    let value = |at: usize| values.get(at).map_or("None", |(written, _, _)| written.as_str()).to_owned();
    let missing = value(3);
    let _ = write!(
        out,
        "struct Holder{id}:\n    items = list[{t}?]()\n\ndef taken{id}(ref items: list[{t}?]) -> {t}??:\n    return \
         items.pop()\n\ndef popped{id}(stdout: Stdout):\n    mut holder = Holder{id}()\n    mut items = list[{t}?]()\n"
    );
    let place = if *in_field { "holder.items" } else { "items" };
    for line in [
        format!("{place}.append({})", value(0)),
        format!("{place}.append(None)"),
        format!("{place}.append({})", value(1)),
    ] {
        let _ = writeln!(out, "    {line}");
    }
    for (n, &(way, which)) in steps.iter().enumerate() {
        let v = value(which);
        let at = |position: &str, name: &str| {
            format!(
                "if len({place}) > 0:\n        {name}{n} = {place}.pop({position}) else {missing}\n        \
                 stdout.print({name}{n})"
            )
        };
        let line = match way {
            PopWay::Pushed => format!("{place}.append({v})"),
            PopWay::PushedNone => format!("{place}.append(None)"),
            PopWay::Kept => format!("got{n} = {place}.pop() else {missing}\n    stdout.print(got{n})"),
            PopWay::Inline => format!("stdout.print({place}.pop() else {missing})"),
            PopWay::Dropped => format!("_ = {place}.pop()"),
            PopWay::First => at("0", "first"),
            PopWay::Middle => at(&format!("len({place}) // 2"), "middle"),
            PopWay::Last => at("-1", "last"),
            PopWay::Removed => {
                format!("removed{n} = {place}.remove({v}) else {missing}\n    stdout.print(removed{n})")
            }
            PopWay::Discarded => format!("{place}.discard({v})"),
            PopWay::DiscardedNone => format!("{place}.discard(None)"),
            PopWay::ByRef => {
                format!("through{n} = taken{id}(ref {place}) else {missing}\n    stdout.print(through{n})")
            }
            PopWay::Empty => {
                format!("mut empty{n} = list[{t}?]()\n    stdout.print(empty{n}.pop() else {missing})")
            }
        };
        let _ = writeln!(out, "    {line}");
    }
    out.push_str("    stdout.print(items)\n    stdout.print(holder)\n\n");
    out
}

/// `sized{id}`, the function a `Sized` calls, after the functions it uses: the number made from
/// what a call gives, so nothing about it is known as it's built, then a line or two for each
/// step, each printing it after.
fn sized_written(sizing: &Sizing) -> String {
    let Sizing { id, num, start, steps } = sizing;
    let (t, convert) = (num.name(), num.convert());
    let start = i64::try_from(*start).unwrap_or(0);
    let mut out = format!(
        "def picked{id}(n: int) -> int:\n    return n\n\ndef parsed{id}(text: str, stdout: Stdout):\n    got = \
         {convert}.parse(text) except why:\n        stdout.print(\"refused\", why)\n        return\n    \
         stdout.print(\"parsed\", got)\n\ndef sized{id}(stdout: Stdout):\n    mut x: {t} = {convert}(picked{id}({start}))\n"
    );
    for step in steps {
        let SizedStep { way, operand } = *step;
        // A literal takes the size it goes into, so one beside `x` needs no conversion, which is
        // the plainer Fennec and keeps a program of hundreds from linting slowly (#2315). One past
        // the largest int can't be written as a literal yet.
        let k = if i64::try_from(operand).is_ok() { operand.to_string() } else { num.literal(operand) };
        let line = match way {
            SizedWay::Added => format!("x += {k}"),
            SizedWay::Taken => format!("x -= {k}"),
            SizedWay::Times => format!("x *= {k}"),
            SizedWay::Halved => format!("x = x // {k}"),
            SizedWay::Remainder => format!("x = x % {k}"),
            SizedWay::WrappingAdd => format!("x = x.wrapping_add({k})"),
            SizedWay::WrappingSub => format!("x = x.wrapping_sub({k})"),
            SizedWay::WrappingMul => format!("x = x.wrapping_mul({k})"),
            SizedWay::SaturatingAdd => format!("x = x.saturating_add({k})"),
            SizedWay::SaturatingSub => format!("x = x.saturating_sub({k})"),
            SizedWay::SaturatingMul => format!("x = x.saturating_mul({k})"),
            SizedWay::Shifted => format!("x = x << {k}"),
            SizedWay::ShiftedDown => format!("x = x >> {k}"),
            SizedWay::Masked => format!("x = x & {k}"),
            SizedWay::Ored => format!("x = x | {k}"),
            SizedWay::Xored => format!("x = x ^ {k}"),
            SizedWay::Inverted => "x = ~x".to_owned(),
            SizedWay::Widened => "stdout.print(\"widened\", int(x) // 3)".to_owned(),
            // A list's items have no size to take from beside them, so this one's written.
            SizedWay::Summed => format!("stdout.print(\"summed\", sum([x, {}]))", num.literal(operand)),
            SizedWay::Looped => format!("for _ in range({operand}):\n        x += 1"),
            SizedWay::Parsed => format!("parsed{id}(\"{operand}\", stdout)"),
        };
        let _ = writeln!(out, "    {line}\n    stdout.print(x)");
    }
    out.push('\n');
    out
}

/// The types and functions an `Arrayed` statement uses, `{N}` standing for its id.
const ARRAY_TYPES: &str = "struct Cell{N}:
    health = 0
    tags = list[str]()

struct Grid{N}:
    counts = array[int, 4]()
    cells = array[Cell{N}, 3]()
    slots = array[Cell{N}?, 2]()
    names = array[str, 2]()
    rows = array[array[int, 2], 2]()

def bumped{N}(ref values: array[int, 4], by: int):
    for ref value in values:
        value += by

def summed{N}(values: array[int, 4]) -> int:
    mut mine = values
    mine[0] = 100
    total = sum(value for value in values)
    return total * 1000 + mine[0]

def at{N}(n: int) -> int:
    return n

";

/// `arrayed{id}`, the function an `Arrayed` calls, after its types and functions: a grid, another,
/// both in a list, the other in a Table, the first in a dict, and a loose array, then a line for
/// each step, then what each holds.
fn arrayed_written(arraying: &Arraying) -> String {
    let id = arraying.id;
    let mut out = ARRAY_TYPES.replace("{N}", &id.to_string());
    let _ = write!(
        out,
        "def arrayed{id}(stdout: Stdout):\n    mut grid = Grid{id}()\n    other = Grid{id}(counts=[1, 2, 3, -4])\n    \
         mut grids = [grid, other]\n    mut table = Table[Grid{id}]()\n    first = table.add(other)\n    mut by_name \
         = {{\"k\": grid}}\n    mut loose = array[int, 4]()\n"
    );
    for (n, step) in arraying.steps.iter().enumerate() {
        let ArrayStep { way, at, value: v, .. } = *step;
        let (a4, a3, a2) = (at % 4, at % 3, at % 2);
        let line = match way {
            ArrayWay::Set => format!("grid.counts[{a4}] = {v}"),
            ArrayWay::Added => format!("grid.counts[{a4}] += {v}"),
            ArrayWay::FromEnd => format!("grid.counts[-{}] += {v}", a4 + 1),
            ArrayWay::Copied => format!(
                "mut copied{n} = grid.counts\n    copied{n}[{a4}] = {v}\n    stdout.print(copied{n}, grid.counts)"
            ),
            ArrayWay::Replaced => format!("grid.counts = loose\n    loose[{a4}] = {v}"),
            ArrayWay::ForRef => format!("for ref each in grid.counts:\n        each += {v}"),
            ArrayWay::ForRefCells => format!(
                "for ref cell in grid.cells:\n        cell.health += {v}\n        cell.tags.append(\"t{v}\")"
            ),
            ArrayWay::CellSet => format!("grid.cells[{a3}].health = {v}"),
            ArrayWay::CellTagged => format!("grid.cells[{a3}].tags.append(\"c{v}\")"),
            ArrayWay::SlotFilled => format!("grid.slots[{a2}] = Cell{id}(health={v})"),
            ArrayWay::SlotCleared => format!("grid.slots[{a2}] = None"),
            ArrayWay::ForRefSlots => format!(
                "for ref slot in grid.slots:\n        if slot is not None:\n            slot.health += {v}"
            ),
            ArrayWay::SlotRead => format!(
                "held{n} = grid.slots[{a2}] else Cell{id}(health={v})\n    stdout.print(grid.slots[{a2}] is None, \
                 held{n}.health)"
            ),
            ArrayWay::InList => format!("grids[{a2}].counts[{a4}] += {v}"),
            ArrayWay::ForRefList => format!("for ref each in grids:\n        each.counts[{a4}] += {v}"),
            ArrayWay::InTable => format!("table[first].counts[{a4}] = {v}"),
            ArrayWay::InDict => format!("by_name[\"k\"].counts[{a4}] += {v}"),
            ArrayWay::ByRef => format!("bumped{id}(ref grid.counts, by={v})"),
            ArrayWay::ByValue => format!("stdout.print(summed{id}(grid.counts))"),
            ArrayWay::Read => format!(
                "stdout.print({v} in grid.counts, len(grid.counts), len(grid.cells), len(grid.slots), grid.counts == \
                 other.counts, list(grid.counts) + [{v}])"
            ),
            ArrayWay::Rows => format!("grid.rows[{a2}][{}] += {v}", (at + 1) % 2),
            ArrayWay::RowCopied => {
                format!(
                    "mut row{n} = grid.rows[{a2}]\n    row{n}[1] = {v}\n    stdout.print(row{n}, grid.rows)"
                )
            }
            ArrayWay::Named => format!("grid.names[{a2}] += \"n{v}\""),
            ArrayWay::WholeCopied => format!(
                "mut whole{n} = grid\n    whole{n}.counts[{a4}] = {v}\n    whole{n}.cells[0].tags.append(\"w\")\n    \
                 stdout.print(whole{n}.counts, grid.counts, whole{n}.cells[0], grid.cells[0])"
            ),
            ArrayWay::Looped => {
                format!("for place in range(at{id}({})):\n        grid.counts[place] += {v}", step.bound())
            }
            ArrayWay::Picked => format!("grid.counts[at{id}({})] = {v}", step.picked()),
        };
        let _ = writeln!(out, "    {line}");
    }
    out.push_str(
        "    stdout.print(grid)\n    stdout.print(other)\n    stdout.print(grids)\n    stdout.print(table[first])\n    \
         stdout.print(by_name)\n    stdout.print(loose)\n\n",
    );
    out
}

/// `generated{id}`, the function a `Generated` calls, after its errors, its failing generator and a
/// function for each step: each step called in turn, its failure printed.
fn generated_written(generating: &Generating) -> String {
    let id = generating.id;
    let mut out = format!(
        "error Shut{id}:\n    code: int\n\nerror Lost{id}:\n    step: int\n\nerror Left{id}:\n    step: int\n\nerror \
         Right{id}:\n    step: int\n\nstruct Boxed{id}[T]:\n    item: T\n\ndef failing{id}(limit: int, fail_at: int) -> \
         Iterator[int] fails Shut{id}:\n    for each in range(limit):\n        if each == fail_at:\n            fail \
         Shut{id}(code=each)\n        yield each\n\n"
    );
    for (n, step) in generating.steps.iter().enumerate() {
        let GenStep { way, limit, fail_at, other_at } = *step;
        let first = format!("failing{id}({limit}, fail_at={fail_at})");
        let second = format!("failing{id}({}, fail_at={other_at})", limit + 1);
        let zipped = |left: &str, right: &str| {
            format!(
                "    for one, other in zip({left}, {right}):\n        stdout.print(\"zip\", one, other)\n"
            )
        };
        let (fails, body) = match way {
            GenWay::ForFrom => (
                format!("Lost{id}"),
                format!(
                    "    for value in {first}? from Lost{id}(step={n}):\n        stdout.print(\"for\", value)\n"
                ),
            ),
            GenWay::EnumerateFrom => (
                format!("Lost{id}"),
                format!(
                    "    for place, value in enumerate({first}? from Lost{id}(step={n})):\n        \
                     stdout.print(\"enumerate\", place, value)\n"
                ),
            ),
            GenWay::ZipBothFrom => (
                format!("Left{id} | Right{id}"),
                zipped(
                    &format!("{first}? from Left{id}(step={n})"),
                    &format!("{second}? from Right{id}(step={n})"),
                ),
            ),
            GenWay::ZipLeftFrom => (
                format!("Left{id} | Shut{id}"),
                zipped(&format!("{first}? from Left{id}(step={n})"), &format!("{second}?")),
            ),
            GenWay::ZipRightFrom => (
                format!("Shut{id} | Right{id}"),
                zipped(&format!("{first}?"), &format!("{second}? from Right{id}(step={n})")),
            ),
            GenWay::ZipListFrom => {
                (format!("Right{id}"), zipped("[10, 20, 30]", &format!("{first}? from Right{id}(step={n})")))
            }
            GenWay::Broken => (
                format!("Lost{id}"),
                format!(
                    "    for value in {first}? from Lost{id}(step={n}):\n        stdout.print(\"broken\", value)\n        if \
                     value == {other_at}:\n            break\n"
                ),
            ),
            GenWay::Named => (
                format!("Shut{id}"),
                format!(
                    "    held = {first}?\n    stdout.print(\"named\")\n    for value in held:\n        \
                     stdout.print(\"named\", value)\n"
                ),
            ),
            GenWay::KeptOver => (
                format!("Shut{id}"),
                format!(
                    "    gens = (failing{id}(size + 1, fail_at={fail_at})? for size in range({limit}))\n    for gen in \
                     gens:\n        for value in gen:\n            stdout.print(\"kept\", value)\n"
                ),
            ),
            GenWay::InTuple => (
                format!("Shut{id}"),
                format!(
                    "    first, second = ({first}?, {second}?)\n    for value in first:\n        \
                     stdout.print(\"first\", value)\n    for value in second:\n        stdout.print(\"second\", value)\n"
                ),
            ),
            GenWay::TupleIndexed => (
                format!("Shut{id}"),
                format!(
                    "    pair = ({first}?, {n})\n    for value in pair[0]:\n        stdout.print(\"indexed\", value, \
                     pair[1])\n"
                ),
            ),
            GenWay::InList => (
                format!("Shut{id}"),
                format!(
                    "    gens = [{first}?, {second}?]\n    for gen in gens:\n        for value in gen:\n            \
                     stdout.print(\"listed\", value)\n"
                ),
            ),
            GenWay::InBox => (
                format!("Shut{id}"),
                format!(
                    "    boxed = Boxed{id}(item={first}?)\n    for value in boxed.item:\n        stdout.print(\"boxed\", \
                     value)\n"
                ),
            ),
        };
        let _ = write!(
            out,
            "def step{id}_{n}(stdout: Stdout) fails {fails}:\n{body}    stdout.print(\"end\", {n})\n\n"
        );
    }
    let _ = writeln!(out, "def generated{id}(stdout: Stdout):");
    for n in 0..generating.steps.len() {
        let _ = writeln!(out, "    step{id}_{n}(stdout) except why:\n        stdout.print(\"failed\", why)");
    }
    out.push('\n');
    out
}

/// `turned{id}`, the function a `Turned` calls: a loop whose int changes sign after its first
/// turn, printed with what each use of it gives.
fn turned_written(turning: &Turning) -> String {
    let Turning { id, how, rounds, start, step, reset_at, reset_to } = *turning;
    let xs: Vec<String> = XS.iter().map(ToString::to_string).collect();
    let (setup, value, after) = match how {
        Turn::StepDown => {
            (format!("    mut at = {start}\n"), "at".to_owned(), format!("        at -= {step}\n"))
        }
        Turn::Flip => (
            format!("    mut sign = 1\n    at = {start}\n    mut v = 0\n"),
            "v".to_owned(),
            "        sign = -sign\n".to_owned(),
        ),
        Turn::Reset => (format!("    mut n = {start}\n"), "n".to_owned(), "        n += 1\n".to_owned()),
    };
    let before = match how {
        Turn::StepDown => String::new(),
        Turn::Flip => "        v = sign * at\n".to_owned(),
        Turn::Reset => format!("        if i == {reset_at}:\n            n = {reset_to}\n"),
    };
    format!(
        "def turned{id}(stdout: Stdout):\n    xs = [{}]\n{setup}    for i in range({rounds}):\n{before}        \
         stdout.print(\"turn\", {value}, xs[{value}], {value} * 3, -9223372036854775804 + {value}, \
         9223372036854775804 + {value})\n{after}\n",
        xs.join(", ")
    )
}

/// The names `Left`'s `ward`s are lent as, outermost first.
pub(crate) const WARDS: [&str; 3] = ["a", "b", "c"];

/// `left{id}`, the function a `Left` calls: a loop whose `with` blocks round `at` leaves by `how`.
fn left_written(id: usize, depth: usize, together: bool, rounds: i64, at: i64, how: Leave) -> String {
    let names = WARDS.get(..depth).unwrap_or(&WARDS);
    let mut out = format!(
        "def left{id}(stdout: Stdout) fails Refused:\n    mut total = 0\n    for i in range({rounds}):\n"
    );
    let mut indent = 2;
    let pad = |indent: usize| "    ".repeat(indent);
    if together {
        let opened: Vec<String> =
            names.iter().map(|name| format!("ward(\"{name}\", stdout) as {name}")).collect();
        let _ = writeln!(out, "{}with {}:", pad(indent), opened.join(", "));
        indent += 1;
    } else {
        for name in names {
            let _ = writeln!(out, "{}with ward(\"{name}\", stdout) as {name}:", pad(indent));
            indent += 1;
        }
    }
    let at_most = at - 1;
    let lines: Vec<String> = match how {
        Leave::ElseReturn => {
            vec![format!("got = present(i, at={at}) else return"), "total = total + got".to_owned()]
        }
        Leave::ElseFail => {
            vec![
                format!("got = present(i, at={at}) else fail Refused(code=i)"),
                "total = total + got".to_owned(),
            ]
        }
        Leave::ElseContinue => {
            vec![format!("got = present(i, at={at}) else continue"), "total = total + got".to_owned()]
        }
        Leave::ElseBreak => {
            vec![format!("got = present(i, at={at}) else break"), "total = total + got".to_owned()]
        }
        Leave::Question => {
            vec![format!("got = capped(i, limit={at_most})?"), "total = total + got".to_owned()]
        }
        Leave::QuestionInside => vec![format!("total = total + capped(i, limit={at_most})? * 2")],
        Leave::ExceptReturn | Leave::ExceptContinue => vec![
            format!("got = capped(i, limit={at_most}) except why:"),
            "    stdout.print(\"caught\", why.code)".to_owned(),
            (if how == Leave::ExceptReturn { "    return" } else { "    continue" }).to_owned(),
            "total = total + got".to_owned(),
        ],
        Leave::ExceptValue => {
            vec![
                format!("got = capped(i, limit={at_most}) except Refused(): -1"),
                "total = total + got".to_owned(),
            ]
        }
        Leave::ExceptArms => vec![
            format!("got = capped(i, limit={at_most}) except why:"),
            "    case _:".to_owned(),
            "        stdout.print(\"caught\", why.code)".to_owned(),
            "        break".to_owned(),
            "total = total + got".to_owned(),
        ],
        Leave::QuestionNestedInner => {
            vec![format!("total = total + capped(capped(i, limit={at_most})?, limit=1000)? * 2")]
        }
        Leave::QuestionNestedOuter => {
            vec![format!("total = total + capped(capped(i, limit=1000)?, limit={at_most})? * 2")]
        }
        Leave::NestedReturn => vec![
            format!("if i >= {at}:"),
            "    match i % 2:".to_owned(),
            "        case 0:".to_owned(),
            "            return".to_owned(),
            "        case _:".to_owned(),
            "            if total > -1:".to_owned(),
            "                return".to_owned(),
            "total = total + i".to_owned(),
        ],
        Leave::Return | Leave::Break | Leave::Continue | Leave::Crash => {
            let leave = match how {
                Leave::Return => "return",
                Leave::Break => "break",
                Leave::Continue => "continue",
                _ => "_ = [1, 2][i + 2]",
            };
            vec![format!("if i == {at}:"), format!("    {leave}"), "total = total + i".to_owned()]
        }
    };
    let _ = writeln!(out, "{}stdout.print(\"in\", {}, i)", pad(indent), names.join(", "));
    for line in lines {
        let _ = writeln!(out, "{}{line}", pad(indent));
    }
    let _ = writeln!(out, "{}stdout.print(\"kept\", i)", pad(indent));
    let _ = write!(out, "        stdout.print(\"round\", i)\n    stdout.print(\"total\", total)\n\n");
    out
}

struct Writer {
    /// `main`'s body.
    out: String,
    /// The functions the statements in `main` call that are written for them, like a lock's
    /// body.
    helpers: String,
    /// How many names this writer has made up, for comprehensions.
    made: usize,
}

/// A number in a lock's body as Fennec.
fn amount(amount: Amount) -> String {
    match amount {
        Amount::Literal(n) => n.to_string(),
        Amount::Sent(by) => format!("(number * {by})"),
    }
}

/// The changes `ops` make under `lock ... as ref held:`, at `indent`.
fn ops_written(ops: &[Op], indent: usize) -> String {
    let mut out = String::new();
    let pad = "    ".repeat(indent);
    for (number, op) in ops.iter().enumerate() {
        let line = match op {
            Op::Append(value) => format!("held.items.append({})", amount(*value)),
            Op::Pop => "_ = held.items.pop()".to_owned(),
            Op::SetItem(at, value, add) => {
                format!("held.items[{at}] {} {}", if *add { "+=" } else { "=" }, amount(*value))
            }
            Op::SetCount(key, value, add) => {
                format!("held.counts[\"{key}\"] {} {}", if *add { "+=" } else { "=" }, amount(*value))
            }
            Op::Remove(key) => format!("_ = held.counts.remove(\"{key}\")"),
            Op::AddTwice(value) => format!("held.add_twice({})", amount(*value)),
            Op::GrowItems(value) => format!("grow_all(ref held.items, {})", amount(*value)),
            Op::RefItem(at, value) => {
                format!(
                    "ref slot{number} = held.items[{at}]\n{pad}slot{number} += {}\n{pad}slot{number} = slot{number} * 2",
                    amount(*value)
                )
            }
            Op::CountsCopied(key, value) => format!(
                "mut kept{number} = held.counts\n{pad}kept{number}[\"{key}\"] = {}\n{pad}held.counts = kept{number}",
                amount(*value)
            ),
            Op::SelfAssigned => format!("held.items = held.items\n{pad}held.counts = held.counts"),
            Op::PopCount(key) => format!("_ = held.counts.pop(\"{key}\")"),
        };
        let _ = writeln!(out, "{pad}{line}");
    }
    out
}

impl Writer {
    /// `call`, a statement that can fail with each of `errors`, none of which has cases, handled
    /// with `except why:` and a `case` for each, every one doing `then`.
    /// `call` with each of `errors` handled: each printing its own word, `crashed` for `Crashed`,
    /// with `DISTINCT_STOPS`, and otherwise all printing `shared`.
    fn stopped(&mut self, indent: usize, call: &str, errors: &[&str], shared: &str) {
        self.line(indent, &format!("{call} except why:"));
        for error in errors {
            let word = if super::generate::DISTINCT_STOPS { error.to_lowercase() } else { shared.to_owned() };
            self.line(indent + 1, &format!("case {error}():"));
            self.line(indent + 2, &format!("stdout.print(\"{word}\")"));
        }
    }

    fn handled(&mut self, indent: usize, call: &str, errors: &[&str], then: &str) {
        self.line(indent, &format!("{call} except why:"));
        for error in errors {
            self.line(indent + 1, &format!("case {error}():"));
            self.line(indent + 2, then);
        }
    }

    fn line(&mut self, indent: usize, text: &str) {
        let _ = writeln!(self.out, "{}{text}", "    ".repeat(indent));
    }

    fn block(&mut self, body: &[Stmt], indent: usize) {
        if body.is_empty() {
            self.line(indent, "pass");
        }
        for stmt in body {
            self.stmt(stmt, indent);
        }
    }

    fn stmt(&mut self, stmt: &Stmt, indent: usize) {
        match stmt {
            Stmt::Let { name, value, changeable, .. } => {
                // A second changeable name for a value is a value of its own, as any `=` makes.
                let written = self.expr(value, false);
                let word = if *changeable { "mut " } else { "" };
                self.line(indent, &format!("{word}{name} = {written}"));
            }
            Stmt::Assign { place, value, add } => {
                let place = match place {
                    Place::Name(name) => name.clone(),
                    Place::Item(name, at) => format!("{name}[{}]", self.expr(at, false)),
                    Place::Field(name, x) => format!("{name}.{}", if *x { "x" } else { "y" }),
                };
                let value = self.expr(value, true);
                let operator = if *add { "+=" } else { "=" };
                self.line(indent, &format!("{place} {operator} {value}"));
            }
            Stmt::Append(name, value) => {
                let value = self.expr(value, true);
                self.line(indent, &format!("{name}.append({value})"));
            }
            Stmt::Pop(name) => self.line(indent, &format!("_ = {name}.pop()")),
            Stmt::If(condition, then, otherwise) => {
                let condition = self.expr(condition, false);
                self.line(indent, &format!("if {condition}:"));
                self.block(then, indent + 1);
                if !otherwise.is_empty() {
                    self.line(indent, "else:");
                    self.block(otherwise, indent + 1);
                }
            }
            Stmt::Range(name, count, body) => {
                let count = self.expr(count, false);
                self.line(indent, &format!("for {name} in range({count}):"));
                self.block(body, indent + 1);
            }
            Stmt::Each(name, list, body) => {
                let list = self.expr(list, false);
                self.line(indent, &format!("for {name} in {list}:"));
                self.block(body, indent + 1);
            }
            Stmt::EachRefRead(name, list, at) => {
                let at = at.map_or_else(|| name.clone(), |at| at.to_string());
                self.line(indent, &format!("for ref {name} in {list}:"));
                self.line(indent + 1, &format!("{name} = {name} + at_or_zero({list}, {at})"));
            }
            Stmt::EachRefThenRead(name, list, at) => {
                self.line(indent, &format!("for ref {name} in {list}:"));
                self.line(indent + 1, &format!("{name} += 1"));
                self.line(indent + 1, &format!("stdout.print(at_or_zero({list}, {at}))"));
            }
            Stmt::EachRef(name, list, by) => {
                let by = self.expr(by, false);
                self.line(indent, &format!("for ref {name} in {list}:"));
                self.line(indent + 1, &format!("{name} += {by}"));
            }
            Stmt::While(counter, limit, body) => {
                self.line(indent, &format!("mut {counter} = 0"));
                self.line(indent, &format!("while {counter} < {limit}:"));
                self.line(indent + 1, &format!("{counter} += 1"));
                self.block(body, indent + 1);
            }
            Stmt::Break(condition) => {
                let condition = self.expr(condition, false);
                self.line(indent, &format!("if {condition}: break"));
            }
            Stmt::Continue(condition) => {
                let condition = self.expr(condition, false);
                self.line(indent, &format!("if {condition}: continue"));
            }
            Stmt::Moved(name, by) => {
                let by = self.expr(by, false);
                self.line(indent, &format!("{name}.moved({by})"));
            }
            Stmt::Print(values) => {
                let values: Vec<String> = values.iter().map(|(value, _)| self.expr(value, false)).collect();
                self.line(indent, &format!("stdout.print({})", values.join(", ")));
            }
            _ => self.held_or_lent(stmt, indent),
        }
    }

    /// The statements about sharing, generators, `with`, `match` and lists changed while they're
    /// counted through: what `stmt` writes for them.
    fn held_or_lent(&mut self, stmt: &Stmt, indent: usize) {
        match stmt {
            Stmt::Indexed(name, list, body) => {
                self.line(indent, &format!("for {name} in range(len({list})):"));
                self.line(indent + 1, &format!("stdout.print({list}[{name}])"));
                self.block(body, indent + 1);
            }
            Stmt::SetKey(name, key, value, add) => {
                let value = self.expr(value, false);
                let operator = if *add { "+=" } else { "=" };
                self.line(indent, &format!("{name}[\"{key}\"] {operator} {value}"));
            }
            Stmt::RemoveKey(name, key) => self.line(indent, &format!("_ = {name}.remove(\"{key}\")")),
            Stmt::Rename(name, text, left) => {
                let (text, left) = (self.expr(text, false), self.expr(left, true));
                self.line(indent, &format!("match {name}:"));
                self.line(indent + 1, "case .Pair(ref left, _, ref label, ref tags):");
                self.line(indent + 2, &format!("label += {text}"));
                self.line(indent + 2, "tags.append(label)");
                self.line(indent + 2, &format!("left = {left}"));
                self.line(indent + 1, "case .Leaf(ref name):");
                self.line(indent + 2, &format!("name += {text}"));
            }
            Stmt::CopiedNested(shape, given, copy, value) => {
                let value = self.expr(value, false);
                let (made, change) = match shape {
                    Nested::DictOfUnits => (
                        "{\"a\": Unit(tags=[1], hp=1), \"b\": Unit(tags=list[int](), hp=2)}",
                        format!("{copy}[\"a\"].tags.append({value})"),
                    ),
                    Nested::ArmyOfUnits => (
                        "Army(units={\"a\": Unit(tags=[1], hp=1)})",
                        format!("{copy}.units[\"a\"].tags.append({value})"),
                    ),
                    Nested::DictOfDicts => (
                        "{\"a\": {\"x\": 1}, \"b\": dict[str, int]()}",
                        format!("{copy}[\"a\"][\"x\"] += {value}"),
                    ),
                    Nested::ListOfDicts => {
                        ("[{\"x\": 1}, {\"y\": 2}]", format!("{copy}[0][\"x\"] += {value}"))
                    }
                    Nested::DictOfUnitLists => {
                        ("{\"a\": [Unit(tags=[1], hp=1)]}", format!("{copy}[\"a\"][0].tags.append({value})"))
                    }
                    Nested::DictOfUnitsHp => {
                        ("{\"a\": Unit(tags=[1], hp=1)}", format!("{copy}[\"a\"].hp += {value}"))
                    }
                    Nested::DictOfIntLists => {
                        ("{\"a\": [1, 2], \"b\": list[int]()}", format!("{copy}[\"a\"].append({value})"))
                    }
                    Nested::ReplacedInCopy => (
                        "{\"a\": Unit(tags=[1], hp=1), \"b\": Unit(tags=list[int](), hp=2)}",
                        format!("{copy}[\"a\"] = Unit(tags=[{value}], hp={value})"),
                    ),
                };
                self.line(indent, &format!("{given} = {made}"));
                self.line(indent, &format!("mut {copy} = {given}"));
                self.line(indent, &change);
                self.line(indent, &format!("stdout.print({given}, {copy})"));
            }
            Stmt::UnionCall(how, members, member, text) => {
                let how = match how {
                    Via::Match => "matched",
                    Via::Is => "checked",
                    Via::Direct => "direct",
                    Via::Rest => "rest",
                    Via::Plain => "plain",
                };
                let made = ["Lite(name=\"a\")", "Pg(host=\"b\")", "Mem(size=1)"]
                    .get(*member)
                    .copied()
                    .unwrap_or_default();
                let text = self.expr(text, false);
                self.line(indent, &format!("stdout.print({how}{members}({made}, {text}))"));
            }
            Stmt::PlanApply(world, spots, xs, changes) => {
                let spots: Vec<String> = spots
                    .iter()
                    .map(|(label, x)| format!("Spot(label=\"{label}\", x={})", self.expr(x, false)))
                    .collect();
                self.line(indent, &format!("mut {world} = [{}]", spots.join(", ")));
                self.made += 1;
                let item = format!("item{}", self.made);
                let xs = self.expr(xs, false);
                self.line(
                    indent,
                    &format!("{changes} = [plan({item}, {world}) for {item} in parallel({xs})]"),
                );
                // `change` is also a function every program has, one taking a `shared` value, which
                // a local of its name once left out of the trimmed program (#1270).
                self.line(indent, &format!("for change in {changes}:"));
                self.line(indent + 1, "match change:");
                self.line(indent + 2, "case .Move(at, by):");
                self.line(indent + 3, &format!("ref spot = {world}[at]"));
                self.line(indent + 3, "spot.x += by");
                self.line(indent + 2, "case .Rename(at, label):");
                self.line(indent + 3, &format!("ref spot = {world}[at]"));
                self.line(indent + 3, "spot.label += label");
                self.line(indent, &format!("stdout.print({world}, len({changes}))"));
            }
            Stmt::Shadowed(id, by) => {
                let _ = write!(
                    self.helpers,
                    "def shadowed{id}(stdout: Stdout, by: int):\n    change = by * 2\n    bump_outer = change + 1\n    \
                     tally = bump_outer - by\n    stdout.print(change, bump_outer, tally)\n\n"
                );
                let by = self.expr(by, false);
                self.line(indent, &format!("shadowed{id}(stdout, by={by})"));
            }
            Stmt::LentRef(list, own) => {
                self.line(indent, &format!("if len({list}) < 64:"));
                self.line(indent + 1, &format!("{own} = {list}"));
                self.line(indent + 1, &format!("appended_all(ref {list}, {own})"));
                self.line(indent + 1, &format!("stdout.print({list}, {own})"));
            }
            Stmt::LentItem(rows, own, items) => {
                let items = self.expr(items, false);
                self.line(indent, &format!("mut {rows} = [{items}, [7]]"));
                self.line(indent, &format!("if len({rows}[0]) < 64:"));
                self.line(indent + 1, &format!("{own} = {rows}[0]"));
                self.line(indent + 1, &format!("appended_all(ref {rows}[0], {own})"));
                self.line(indent + 1, &format!("stdout.print({rows}, {own})"));
            }
            Stmt::SelfField(holder) => {
                self.line(indent, &format!("{holder}.items = {holder}.items"));
                self.line(indent, &format!("{holder}.tag = {holder}.tag"));
            }
            Stmt::GrownBy(holder, own) => {
                self.line(indent, &format!("if len({holder}.items) < 64:"));
                self.line(indent + 1, &format!("{holder}.items.append({holder}.tag)"));
                self.line(indent + 1, &format!("{own} = {holder}.items"));
                self.line(indent + 1, &format!("{holder}.grown_by({own})"));
                self.line(indent + 1, &format!("stdout.print({holder}, {own})"));
            }
            Stmt::LocalRef(name, place, body) => {
                let place = match place {
                    RefPlace::Item(list, at) => format!("{list}[{}]", self.expr(at, false)),
                    RefPlace::Field(point, x) => format!("{point}.{}", if *x { "x" } else { "y" }),
                    RefPlace::HeldItem(holder, at) => format!("{holder}.items[{}]", self.expr(at, false)),
                    RefPlace::HeldList(holder) => format!("{holder}.items"),
                    RefPlace::Tag(holder) => format!("{holder}.tag"),
                    RefPlace::NodeItem(nodes, at) => format!("{nodes}[{}]", self.expr(at, false)),
                };
                self.line(indent, &format!("ref {name} = {place}"));
                for stmt in body {
                    self.stmt(stmt, indent);
                }
            }
            Stmt::NodesAppend(name, node) => {
                let node = self.expr(node, false);
                self.line(indent, &format!("{name}.append({node})"));
            }
            Stmt::NodesPop(name) => self.line(indent, &format!("_ = {name}.pop()")),
            Stmt::NodesSet(name, at, node) => {
                let (at, node) = (self.expr(at, false), self.expr(node, true));
                self.line(indent, &format!("if len({name}) > 0: {name}[{at} % len({name})] = {node}"));
            }
            Stmt::Reshape(name, text, child) => {
                let (text, child) = (self.expr(text, false), self.expr(child, false));
                self.line(indent, &format!("match {name}:"));
                self.line(indent + 1, "case .Branch(ref label, ref children):");
                self.line(indent + 2, &format!("label += {text}"));
                self.line(indent + 2, &format!("children.append({child})"));
                self.line(indent + 1, "case .Tagged(ref tags, ref inner):");
                self.line(indent + 2, &format!("tags.append({text})"));
                self.line(indent + 2, &format!("inner.append({child})"));
                self.line(indent + 1, "case .Leaf(ref name):");
                self.line(indent + 2, &format!("name += {text}"));
                self.line(indent + 1, "case .Number(ref value):");
                self.line(indent + 2, "value += 1");
            }
            Stmt::Buffered(name, op) => {
                let line = match op {
                    BufOp::Push(value) => format!("{name}.push({})", self.expr(value, false)),
                    BufOp::Pop => format!("_ = {name}.pop()"),
                    BufOp::Set(at, value) => {
                        let (at, value) = (self.expr(at, false), self.expr(value, false));
                        format!("{name}[{at}] = {value}")
                    }
                    BufOp::Insert(at, value) => {
                        let (at, value) = (self.expr(at, false), self.expr(value, false));
                        format!("{name}.insert({at}, {value})")
                    }
                    BufOp::Remove(at) => format!("_ = {name}.remove({})", self.expr(at, false)),
                    BufOp::Truncate(count) => format!("{name}.truncate({})", self.expr(count, false)),
                    BufOp::Extend(other) => format!("{name}.extend({other})"),
                };
                self.line(indent, &line);
            }
            Stmt::HolderAppend(name, value) => {
                let value = self.expr(value, false);
                self.line(indent, &format!("{name}.items.append({value})"));
            }
            Stmt::HolderTag(name, value) => {
                let value = self.expr(value, false);
                self.line(indent, &format!("{name}.tag += {value}"));
            }
            Stmt::PutList(name, key, list) => {
                let list = self.expr(list, true);
                self.line(indent, &format!("{name}[\"{key}\"] = {list}"));
            }
            Stmt::ListAppend(name, key, value) => {
                let value = self.expr(value, false);
                self.line(indent, &format!("if \"{key}\" in {name}: {name}[\"{key}\"].append({value})"));
            }
            Stmt::GrowAll(list, by) => {
                let by = self.expr(by, false);
                self.line(indent, &format!("grow_all(ref {list}, {by})"));
            }
            Stmt::Grow(name, by) => {
                let by = self.expr(by, false);
                self.line(indent, &format!("match {name}:"));
                self.line(indent + 1, "case .Circle(ref radius):");
                self.line(indent + 2, &format!("radius += {by}"));
                self.line(indent + 1, "case .Square(ref side):");
                self.line(indent + 2, &format!("side += {by}"));
                self.line(indent + 1, "case .Empty:");
                self.line(indent + 2, "pass");
            }
            Stmt::Share(name, x, y) => {
                let (x, y) = (self.expr(x, false), self.expr(y, false));
                self.line(indent, &format!("shared {name} = Point(x={x}, y={y})"));
            }
            Stmt::Change(name, by, refuse) => {
                let (by, refuse) = (self.expr(by, false), self.expr(refuse, false));
                self.handled(
                    indent,
                    &format!("change({name}, by={by}, refuse={refuse})"),
                    &["Refused"],
                    "stdout.print(\"refused\")",
                );
            }
            Stmt::Tally(name, items) => {
                let items = self.expr(items, false);
                self.stopped(
                    indent,
                    &format!("tally({name}, items={items})"),
                    &["Crashed", "Closed", "Timeout"],
                    "crashed",
                );
            }
            Stmt::PrintShared(name) => {
                self.line(indent, &format!("lock {name} as held:"));
                self.line(indent + 1, "stdout.print(held)");
            }
            Stmt::EachCounted(name, limit, body) => {
                let limit = self.expr(limit, false);
                self.line(indent, &format!("for {name} in counted({limit}):"));
                self.block(body, indent + 1);
            }
            Stmt::With(name, start, body) => {
                let start = self.expr(start, false);
                self.line(indent, &format!("with tallied({start}, stdout) as {name}:"));
                self.block(body, indent + 1);
            }
            _ => self.bagged(stmt, indent),
        }
    }

    /// The statements about a `shared Bag`: what `stmt` writes for them, with the functions
    /// they call written alongside.
    fn bagged(&mut self, stmt: &Stmt, indent: usize) {
        match stmt {
            Stmt::PrintOuter(name) => {
                self.line(indent, &format!("lock {name} as held:"));
                self.line(indent + 1, "stdout.print(held.n)");
            }
            Stmt::TableRound { table, ids, xs, removed, read, by } => {
                self.line(indent, &format!("mut {table} = Table[Point]()"));
                for (id, x) in ids.iter().zip(xs) {
                    let x = self.expr(x, false);
                    self.line(indent, &format!("{id} = {table}.add(Point(x={x}, y=0))"));
                }
                if let Some(id) = removed.and_then(|at| ids.get(at)) {
                    self.line(indent, &format!("_ = {table}.remove({id})"));
                }
                let by = self.expr(by, false);
                let read = ids.get(*read).cloned().unwrap_or_default();
                self.line(indent, &format!("for ref point in {table}:"));
                self.line(indent + 1, &format!("point.x += {by}"));
                self.line(indent + 1, &format!("stdout.print({table}[{read}].x)"));
                self.line(indent, &format!("stdout.print({table})"));
            }
            Stmt::ShareOuter(name, bag) => {
                self.line(indent, &format!("shared {name} = Outer(inner={bag}, n=0)"));
            }
            Stmt::OuterBump(name, by, refuse) => {
                let (by, refuse) = (self.expr(by, false), self.expr(refuse, false));
                self.handled(
                    indent,
                    &format!("bump_outer({name}, by={by}, refuse={refuse})"),
                    &["Refused"],
                    "stdout.print(\"refused\")",
                );
            }
            Stmt::Summed(limit, stop) => {
                let (limit, stop) = (self.expr(limit, false), self.expr(stop, false));
                self.line(
                    indent,
                    &format!("stdout.print(summed_shown({limit}, stop={stop}, stdout=stdout))"),
                );
            }
            Stmt::ZipSummed(zipped) => {
                let words: Vec<String> = (1..=zipped.words).map(|word| word.to_string()).collect();
                let (way, limit, stop) = (zipped.number(), zipped.limit, zipped.stop);
                let words = if words.is_empty() {
                    "list[int]()".to_owned()
                } else {
                    format!("[{}]", words.join(", "))
                };
                self.line(
                    indent,
                    &format!(
                        "stdout.print(through_shown(way={way}, limit={limit}, stop={stop}, words={words}, stdout=stdout))"
                    ),
                );
            }
            Stmt::Arrowed(how, n, limit) => {
                let n = self.expr(n, false);
                let capped = format!("capped({n}, limit={limit})?");
                match how {
                    Arrow::Tried => self.line(indent, &format!("stdout.print(tried(() => {capped}))")),
                    Arrow::Twice => {
                        // A failing call can't sit inside another expression, so it has a line.
                        self.made += 1;
                        let name = format!("twice{}", self.made);
                        self.line(
                            indent,
                            &format!("{name} = tried_twice(() => {capped}) except Refused(): \"refused\""),
                        );
                        self.line(indent, &format!("stdout.print({name})"));
                    }
                    Arrow::Fallback => self.line(indent, &format!("stdout.print(fallback(() => {capped}))")),
                    Arrow::Each(list) => {
                        let list = self.expr(list, false);
                        self.line(
                            indent,
                            &format!(
                                "stdout.print(tried_each({list}, item => capped(item, limit={limit})?))"
                            ),
                        );
                    }
                    Arrow::RanFine => self.line(indent, &format!("stdout.print(ran_fine(() => {capped}))")),
                    Arrow::Either(other) => {
                        let other = self.expr(other, false);
                        self.line(
                            indent,
                            &format!(
                                "stdout.print(either(() => paired({capped}, shut_below({other}, limit={limit})?)))"
                            ),
                        );
                    }
                }
            }
            Stmt::Guarded { code, refuse, crash, noted } => {
                let shown = |on: bool| if on { "True" } else { "False" };
                let (refuse, crash, noted) = (shown(*refuse), shown(*crash), shown(*noted));
                self.line(
                    indent,
                    &format!(
                        "stdout.print(guarded_shown({code}, refuse={refuse}, crash={crash}, noted={noted}, \
                         stdout=stdout))"
                    ),
                );
            }
            Stmt::GuardAll(items) => {
                let items: Vec<String> = items.iter().map(ToString::to_string).collect();
                let call = format!("guard_all([{}], stdout)", items.join(", "));
                self.stopped(indent, &call, &["Crashed", "Closed", "Timeout", "Refused"], "stopped");
            }
            Stmt::Streamed(streaming) => {
                let _ = write!(self.helpers, "{}", streamed_written(streaming));
                self.line(indent, &format!("streamed{}(stdout)", streaming.id));
            }
            Stmt::Woken(waking) => {
                let _ = write!(self.helpers, "{}", woken_written(waking));
                self.line(indent, &format!("for _ in range({}):", waking.rounds));
                let indent = indent + 1;
                self.line(indent, &format!("woken{}(stdout) except why:", waking.id));
                for (case, then) in [
                    ("Crashed()", "stdout.print(\"crashed\")"),
                    ("Closed()", "stdout.print(\"closed\")"),
                    ("Timeout()", "stdout.print(\"timeout\")"),
                    ("Refused(code=code)", "stdout.print(\"refused\", code)"),
                ] {
                    self.line(indent + 1, &format!("case {case}:"));
                    self.line(indent + 2, then);
                }
            }
            Stmt::Swapped(swapping) => {
                let _ = write!(self.helpers, "{}", swapped_written(swapping));
                self.line(indent, &format!("swaps{}(stdout)", swapping.id));
            }
            Stmt::Shelved(shelving) => {
                let _ = write!(self.helpers, "{}", shelved_written(shelving));
                self.line(indent, &format!("shelved{}(stdout)", shelving.id));
            }
            Stmt::RefCased(casing) => {
                let _ = write!(self.helpers, "{}", ref_cased_written(casing));
                self.line(indent, &format!("ref_cased{}(stdout)", casing.id));
            }
            Stmt::Captured(capturing) => {
                let _ = write!(self.helpers, "{}", captured_written(capturing));
                self.line(indent, &format!("captured{}(stdout)", capturing.id));
            }
            Stmt::Narrowed(narrowing) => {
                let _ = write!(self.helpers, "{}", narrowed_written(narrowing));
                self.line(indent, &format!("narrowed{}(stdout)", narrowing.id));
            }
            Stmt::Listened(listening) => {
                let _ = write!(self.helpers, "{}", listened_written(listening));
                self.stopped(
                    indent,
                    &format!("listened{}(stdout)", listening.id),
                    &["Crashed", "Closed", "Timeout"],
                    "crashed",
                );
            }
            Stmt::Yielded(yielding) => {
                let _ = write!(self.helpers, "{}", yielded_written(yielding));
                self.stopped(
                    indent,
                    &format!("yielded{}(stdout)", yielding.id),
                    &["Crashed", "Closed", "Timeout"],
                    "crashed",
                );
            }
            Stmt::FieldCalls(calls) => {
                let _ = write!(self.helpers, "{}", field_calls_written(calls));
                self.line(indent, &format!("fielded{}(stdout)", calls.id));
            }
            Stmt::Constants(constants) => {
                let _ = write!(self.helpers, "{}", constants_written(constants));
                self.handled(
                    indent,
                    &format!("constants{}(stdout)", constants.id),
                    &["Crashed"],
                    "stdout.print(\"crashed\")",
                );
            }
            Stmt::Aliased(aliasing) => {
                let _ = write!(self.helpers, "{}", aliased_written(aliasing));
                self.line(indent, &format!("aliased{}(stdout)", aliasing.id));
            }
            Stmt::Turned(turning) => {
                let _ = write!(self.helpers, "{}", turned_written(turning));
                self.line(indent, &format!("turned{}(stdout)", turning.id));
            }
            Stmt::Filed(filing) => {
                let _ = write!(self.helpers, "{}", filed_written(filing));
                self.line(indent, &format!("filed{}(fs, stdout)", filing.id));
            }
            Stmt::Generic(generic) => {
                let _ = write!(self.helpers, "{}", generic_written(generic));
                self.line(indent, &format!("generic{}(stdout)", generic.id));
            }
            Stmt::Stuck(sticking) => {
                let _ = write!(self.helpers, "{}", stuck_written(*sticking));
                self.line(indent, &format!("stuck{}(stdout) except why:", sticking.id));
                self.line(indent + 1, "stdout.print(\"failed\", why)");
            }
            Stmt::Interfaced(interfacing) => {
                let _ = write!(self.helpers, "{}", interfaced_written(interfacing));
                self.line(indent, &format!("interfaced{}(stdout)", interfacing.id));
            }
            Stmt::LockedTogether(locking) => {
                let _ = write!(self.helpers, "{}", locked_together_written(locking));
                self.line(indent, &format!("locked_together{}(stdout)", locking.id));
            }
            Stmt::Arrayed(arraying) => {
                let _ = write!(self.helpers, "{}", arrayed_written(arraying));
                self.line(indent, &format!("arrayed{}(stdout)", arraying.id));
            }
            Stmt::Popped(popping) => {
                let _ = write!(self.helpers, "{}", popped_written(popping));
                self.line(indent, &format!("popped{}(stdout)", popping.id));
            }
            Stmt::Sized(sizing) => {
                let _ = write!(self.helpers, "{}", sized_written(sizing));
                self.line(indent, &format!("sized{}(stdout)", sizing.id));
            }
            Stmt::Generated(generating) => {
                let _ = write!(self.helpers, "{}", generated_written(generating));
                self.line(indent, &format!("generated{}(stdout)", generating.id));
            }
            Stmt::Saved(saving) => {
                let _ = write!(self.helpers, "{}", super::saving::written(saving));
                self.line(indent, &format!("saved{}(stdout)", saving.id));
            }
            Stmt::Stored(storing) => {
                let _ = write!(self.helpers, "{}", super::storing::written(storing));
                self.line(indent, &format!("stored{}(fs, stdout) except why:", storing.id));
                self.line(indent + 1, "stdout.print(\"stored failed\", why.message())");
            }
            Stmt::Crewed(crew) => {
                let _ = write!(self.helpers, "{}", super::crews::written(crew));
                self.line(indent, &format!("crewed{}(stdout)", crew.id));
            }
            Stmt::Pooled(pooling) => {
                let _ = write!(self.helpers, "{}", super::pools::written(pooling));
                self.line(indent, &format!("pooled{}(stdout)", pooling.id));
            }
            Stmt::Messaged(messaging) => {
                let _ = write!(self.helpers, "{}", super::messaging::written(messaging));
                self.line(indent, &format!("messaged{}(stdout) except why:", messaging.id));
                self.line(indent + 1, "stdout.print(\"messaged failed\", why)");
            }
            Stmt::Opened(opening) => {
                let _ = write!(self.helpers, "{}", super::opening::written(opening));
                self.line(indent, &format!("opened{}(stdout) except why:", opening.id));
                self.line(indent + 1, "stdout.print(\"refused\", why.code)");
            }
            Stmt::Ruled(ruling) => {
                let _ = write!(self.helpers, "{}", ruled_written(ruling));
                self.line(indent, &format!("ruled{}(stdout)", ruling.id));
            }
            Stmt::Ordered(in_order) => {
                let _ = write!(self.helpers, "{}", ordered_written(in_order));
                self.line(indent, &format!("ordered{}(stdout)", in_order.id));
            }
            Stmt::Owned(owning) => {
                let _ = write!(self.helpers, "{}", owned_written(owning));
                self.line(indent, &format!("owned{}(stdout)", owning.id));
            }
            Stmt::Left { id, depth, together, rounds, at, how } => {
                let _ = write!(self.helpers, "{}", left_written(*id, *depth, *together, *rounds, *at, *how));
                self.line(indent, &format!("left{id}(stdout) except why:"));
                self.line(indent + 1, "stdout.print(\"refused\", why.code)");
            }
            Stmt::FloatKeys { held, names: (keyed, seen), values, probe, asked, pair: (left, right) } => {
                let ty = match held {
                    FloatHeld::Gauge => "Gauge",
                    FloatHeld::F32Gauge => "F32Gauge",
                    FloatHeld::Level => "Level",
                    FloatHeld::Level32 => "Level32",
                    FloatHeld::Marked => "Marked",
                };
                self.line(indent, &format!("mut {keyed} = dict[{ty}, int]()"));
                for (at, value) in values.iter().enumerate() {
                    self.line(indent, &format!("{keyed}[{}] = {at}", float_held(*held, *value, false)));
                }
                let mut items: Vec<String> =
                    values.iter().map(|value| float_held(*held, *value, false)).collect();
                if matches!(held, FloatHeld::Marked) {
                    items.push("\"a\"".to_owned());
                }
                let typed = if matches!(held, FloatHeld::Marked) { ": set[Marked]" } else { "" };
                self.line(indent, &format!("{seen}{typed} = {{{}}}", items.join(", ")));
                let at = |place: &usize| values.get(*place).copied().unwrap_or(0);
                let probe = float_held(*held, at(probe), false);
                let asked = float_held(*held, *asked, false);
                let (left, right) = (float_held(*held, at(left), false), float_held(*held, at(right), true));
                self.line(
                    indent,
                    &format!(
                        "stdout.print(len({keyed}), {keyed}[{probe}], len({seen}), {asked} in {seen}, \
                         {left} == {right}, {left} != {right})"
                    ),
                );
            }
            Stmt::ShareBag(name, items, counts) => {
                let items = self.expr(items, false);
                let counts = if counts.is_empty() {
                    "dict[str, int]()".to_owned()
                } else {
                    let pairs: Vec<String> =
                        counts.iter().map(|(key, n)| format!("\"{key}\": {n}")).collect();
                    format!("{{{}}}", pairs.join(", "))
                };
                self.line(indent, &format!("shared {name} = Bag(items={items}, counts={counts})"));
            }
            Stmt::Locked { id, name, ops, refuse } => {
                let _ = write!(
                    self.helpers,
                    "def locked{id}(box: shared Bag, refuse: bool) fails Refused:\n    lock box as ref held:\n{}        \
                     if refuse:\n            fail Refused(code={id})\n\n",
                    ops_written(ops, 2)
                );
                let refuse = self.expr(refuse, false);
                self.handled(
                    indent,
                    &format!("locked{id}({name}, refuse={refuse})"),
                    &["Refused"],
                    "stdout.print(\"refused\")",
                );
            }
            Stmt::Sent { id, name, ops, items } => {
                let _ = write!(
                    self.helpers,
                    "def worker{id}(inbox: Inbox[int], box: shared Bag):\n    for number in inbox:\n        \
                     lock box as ref held:\n{}\ndef sent{id}(box: shared Bag, items: list[int]) fails Crashed | Closed | \
                     Timeout:\n    with launch((inbox) => worker{id}(inbox, box))? as sends:\n        for item in items:\n            \
                     sends.send(item)?\n\n",
                    ops_written(ops, 3)
                );
                let items = self.expr(items, false);
                self.stopped(
                    indent,
                    &format!("sent{id}({name}, items={items})"),
                    &["Crashed", "Closed", "Timeout"],
                    "crashed",
                );
            }
            _ => {}
        }
    }

    /// An expression, with `known` saying whether the type it's going into is known there, which
    /// makes a shape's case written with a leading dot.
    fn expr(&mut self, expr: &Expr, known: bool) -> String {
        match expr {
            Expr::Int(n) => n.to_string(),
            Expr::Bool(value) => (if *value { "True" } else { "False" }).to_owned(),
            Expr::Str(text) => format!("\"{text}\""),
            Expr::Name(name) => name.clone(),
            Expr::Arith(operator, left, right) => {
                let symbol = match operator {
                    Arith::Add => "+",
                    Arith::Sub => "-",
                    Arith::Mul => "*",
                    Arith::Div => "//",
                    Arith::Rem => "%",
                };
                format!("({} {symbol} {})", self.expr(left, false), self.expr(right, false))
            }
            Expr::Small(value) => format!("({} % 1009)", self.expr(value, false)),
            Expr::Compare(compare, left, right) | Expr::FloatCompare(compare, left, right) => {
                let symbol = match compare {
                    Compare::Less => "<",
                    Compare::LessEq => "<=",
                    Compare::Equal => "==",
                    Compare::NotEqual => "!=",
                    Compare::Greater => ">",
                };
                format!("({} {symbol} {})", self.expr(left, false), self.expr(right, false))
            }
            Expr::And(left, right) => format!("({} and {})", self.expr(left, false), self.expr(right, false)),
            Expr::Not(value) => format!("(not {})", self.expr(value, false)),
            Expr::Choose(when, yes, no) => {
                let (yes, when, no) = (self.expr(yes, false), self.expr(when, false), self.expr(no, false));
                format!("({yes} if {when} else {no})")
            }
            Expr::Concat(left, right) => {
                format!("({} + {})", self.expr(left, false), self.expr(right, false))
            }
            Expr::Format(value) => format!("f\"<{{{}}}>\"", self.expr(value, false)),
            Expr::TextLen(text) => format!("len({})", self.expr(text, false)),
            Expr::Ints(items) if items.is_empty() => "list[int]()".to_owned(),
            Expr::Ints(items) => {
                let items: Vec<String> = items.iter().map(|item| self.expr(item, false)).collect();
                format!("[{}]", items.join(", "))
            }
            Expr::Mapped(list, by, plus, parallel) => {
                self.made += 1;
                let item = format!("item{}", self.made);
                let (by, plus, list) = (self.expr(by, false), self.expr(plus, false), self.expr(list, false));
                let list = if *parallel { format!("parallel({list})") } else { list };
                format!("[{item} * {by} + {plus} for {item} in {list}]")
            }
            Expr::Regrown(list) => {
                self.made += 1;
                let item = format!("item{}", self.made);
                format!("sum(regrown({item}) for {item} in parallel({}))", self.expr(list, false))
            }
            Expr::ParallelSum(list, by) => {
                self.made += 1;
                let item = format!("item{}", self.made);
                let (by, list) = (self.expr(by, false), self.expr(list, false));
                format!("sum({item} * {by} for {item} in parallel({list}))")
            }
            Expr::FloatSum(list, scale, filtered) => {
                self.made += 1;
                let item = format!("item{}", self.made);
                let (scale, list) = (self.expr(scale, false), self.expr(list, false));
                let kept = if *filtered { format!(" if {item} % 2 == 0") } else { String::new() };
                format!("sum(float({item}) * {scale} for {item} in parallel({list}){kept})")
            }
            Expr::Upto(count) => format!("list(range({count}))"),
            Expr::Len(list) => format!("len({})", self.expr(list, false)),
            Expr::Index(list, at) => format!("{}[{}]", self.expr(list, false), self.expr(at, false)),
            Expr::AtOrZero(list, at) => {
                format!("at_or_zero({}, {})", self.expr(list, false), self.expr(at, false))
            }
            Expr::Sum(list) => format!("sum({})", self.expr(list, false)),
            Expr::Point(x, y) => format!("Point(x={}, y={})", self.expr(x, false), self.expr(y, false)),
            Expr::Field(point, x) => format!("{}.{}", self.expr(point, false), if *x { "x" } else { "y" }),
            Expr::Total(point) => format!("{}.total()", self.expr(point, false)),
            Expr::Shape(case, number) => {
                let start = if known { "." } else { "Shape." };
                match case {
                    Case::Circle => format!("{start}Circle(radius={})", self.expr(number, false)),
                    Case::Square => format!("{start}Square(side={})", self.expr(number, false)),
                    Case::Empty => format!("{start}Empty"),
                }
            }
            Expr::Area(shape) => format!("area({})", self.expr(shape, true)),
            Expr::Bump(name, by) => format!("bump(ref {name}, {})", self.expr(by, false)),
            Expr::Convert(size, value) => format!("{}({})", size.name(), self.expr(value, false)),
            Expr::SizedArith(_, operator, left, right) => {
                let symbol = match operator {
                    Arith::Sub => "-",
                    Arith::Mul => "*",
                    _ => "+",
                };
                format!("({} {symbol} {})", self.expr(left, false), self.expr(right, false))
            }
            Expr::SizedWrap(_, left, right) => {
                format!("{}.wrapping_add({})", self.expr(left, false), self.expr(right, false))
            }
            Expr::IntOf(byte) => format!("int({})", self.expr(byte, false)),
            Expr::SumCounted(limit) => format!("sum(counted({}))", self.expr(limit, false)),
            Expr::Count(name) => format!("{name}.count"),
            Expr::Holder(items, tag) => {
                format!("Holder(items={}, tag={})", self.expr(items, true), self.expr(tag, false))
            }
            Expr::HeldItems(holder) => format!("{}.items", self.expr(holder, false)),
            Expr::Tag(holder) => format!("{}.tag", self.expr(holder, false)),
            Expr::Lists(pairs) if pairs.is_empty() => "dict[str, list[int]]()".to_owned(),
            Expr::Lists(pairs) => {
                let pairs: Vec<String> =
                    pairs.iter().map(|(key, list)| format!("\"{key}\": {}", self.expr(list, true))).collect();
                format!("{{{}}}", pairs.join(", "))
            }
            Expr::ListAt(lists, key) => {
                format!("({}.get(\"{key}\") else list[int]())", self.expr(lists, false))
            }
            Expr::Summing(name) => format!("() => sum({name})"),
            Expr::Called(name) => format!("{name}()"),
            Expr::Counts(pairs) if pairs.is_empty() => "dict[str, int]()".to_owned(),
            Expr::Counts(pairs) => {
                let pairs: Vec<String> =
                    pairs.iter().map(|(key, n)| format!("\"{key}\": {}", self.expr(n, false))).collect();
                format!("{{{}}}", pairs.join(", "))
            }
            Expr::CountOf(counts, key) => format!("({}.get(\"{key}\") else 0)", self.expr(counts, false)),
            Expr::Has(counts, key) => format!("(\"{key}\" in {})", self.expr(counts, false)),
            Expr::Buffer(capacity, items) => {
                let made = capacity
                    .map_or_else(|| "Buffer[int]()".to_owned(), |n| format!("Buffer[int](capacity={n})"));
                if items.is_empty() {
                    return made;
                }
                // Filled through a function, since a buffer has no literal.
                let items: Vec<String> = items.iter().map(|item| self.expr(item, false)).collect();
                format!("buffer_of({made}, [{}])", items.join(", "))
            }
            Expr::BufItems(buffer) => {
                self.made += 1;
                let item = format!("item{}", self.made);
                format!("[{item} for {item} in {}]", self.expr(buffer, false))
            }
            Expr::BufLen(buffer) => format!("len({})", self.expr(buffer, false)),
            Expr::BufAt(buffer, at) => format!("{}[{}]", self.expr(buffer, false), self.expr(at, false)),
            Expr::Leaf(name) => {
                let start = if known { "." } else { "Node." };
                format!("{start}Leaf(name={})", self.expr(name, false))
            }
            Expr::Number(value) => {
                let start = if known { "." } else { "Node." };
                format!("{start}Number(value={})", self.expr(value, false))
            }
            Expr::Branch(label, children) => {
                let start = if known { "." } else { "Node." };
                format!(
                    "{start}Branch(label={}, children={})",
                    self.expr(label, false),
                    self.expr(children, false)
                )
            }
            Expr::Tagged(tags, inner) => {
                let start = if known { "." } else { "Node." };
                format!("{start}Tagged(tags={}, inner={})", self.expr(tags, false), self.expr(inner, false))
            }
            Expr::Texts(texts) if texts.is_empty() => "list[str]()".to_owned(),
            Expr::Texts(texts) => {
                let texts: Vec<String> = texts.iter().map(|text| self.expr(text, false)).collect();
                format!("[{}]", texts.join(", "))
            }
            Expr::NodeList(nodes) if nodes.is_empty() => "list[Node]()".to_owned(),
            Expr::NodeList(nodes) => {
                let nodes: Vec<String> = nodes.iter().map(|node| self.expr(node, false)).collect();
                format!("[{}]", nodes.join(", "))
            }
            Expr::NodeAt(nodes, at) => {
                format!("node_or_leaf({}, {})", self.expr(nodes, false), self.expr(at, false))
            }
            Expr::Digest(node) => format!("digest({})", self.expr(node, true)),
            Expr::DigestAll(nodes) => format!("digest_all({})", self.expr(nodes, false)),
            Expr::Deep(depth, label) => format!("deep({depth}, {})", self.expr(label, false)),
            Expr::NamedLeaf(name) => {
                let start = if known { "." } else { "Named." };
                format!("{start}Leaf(name={})", self.expr(name, false))
            }
            Expr::NamedPair(left, right, label, tags) => {
                let start = if known { "." } else { "Named." };
                format!(
                    "{start}Pair(left={}, right={}, label={}, tags={})",
                    self.expr(left, true),
                    self.expr(right, true),
                    self.expr(label, false),
                    self.expr(tags, false)
                )
            }
            Expr::Chain(depth, label) => format!("chain({depth}, {})", self.expr(label, false)),
            Expr::NamedDigest(named) => format!("named_digest({})", self.expr(named, true)),
            Expr::NamedDepth(named) => format!("named_depth({})", self.expr(named, true)),
            Expr::DepthOf(node) => format!("depth_of({})", self.expr(node, true)),
            Expr::Float(x) => super::eval::float_text(*x),
            Expr::FloatArith(operator, left, right) => {
                let symbol = match operator {
                    Arith::Add => "+",
                    Arith::Sub => "-",
                    Arith::Mul => "*",
                    Arith::Div | Arith::Rem => "/",
                };
                format!("({} {symbol} {})", self.expr(left, false), self.expr(right, false))
            }
            Expr::FloatOf(value) => format!("float({})", self.expr(value, false)),
            Expr::Truncated(value) => format!("int({})", self.expr(value, false)),
        }
    }
}

/// The value holding `FLOATS[value]`, `Off` only for an enum. On the right of `==`, an enum's case
/// leaves its type out, as the linter has it.
fn float_held(held: FloatHeld, value: usize, right: bool) -> String {
    let (float, _) = FLOATS.get(value).copied().unwrap_or(("0.0", 0));
    let enum_named = |name: &str| if right { String::new() } else { name.to_owned() };
    match held {
        FloatHeld::Gauge | FloatHeld::Marked => format!("Gauge(at={float}, n=1)"),
        FloatHeld::F32Gauge => format!("F32Gauge(at=f32({float}))"),
        FloatHeld::Level if float == "Off" => format!("{}.Off", enum_named("Level")),
        FloatHeld::Level => format!("{}.At(value={float})", enum_named("Level")),
        FloatHeld::Level32 if float == "Off" => format!("{}.Off", enum_named("Level32")),
        FloatHeld::Level32 => format!("{}.At(value=f32({float}))", enum_named("Level32")),
    }
}
