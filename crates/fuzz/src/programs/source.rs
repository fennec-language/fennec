//! A program tree written out as Fennec.

#![expect(clippy::too_many_lines, reason = "one match writes each statement, and one each expression")]

use std::fmt::Write as _;

use super::ast::{
    Amount, Arith, Arrow, BufOp, Case, Compare, Expr, FLOATS, FloatHeld, Leave, Nested, Op, Place, Program,
    RefPlace, Stmt, StreamMap, StreamRead, Streaming, Turn, Turning, Via, XS, YS,
};

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

struct Gauge32:
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
    format!("{HEADER}{tasks}{}def main(stdout: Stdout):\n{}", writer.helpers, writer.out)
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
                self.handled(
                    indent,
                    &format!("tally({name}, items={items})"),
                    &["Crashed", "Closed", "Timeout"],
                    "stdout.print(\"crashed\")",
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
                self.handled(
                    indent,
                    &call,
                    &["Crashed", "Closed", "Timeout", "Refused"],
                    "stdout.print(\"stopped\")",
                );
            }
            Stmt::Streamed(streaming) => {
                let _ = write!(self.helpers, "{}", streamed_written(streaming));
                self.line(indent, &format!("streamed{}(stdout)", streaming.id));
            }
            Stmt::Turned(turning) => {
                let _ = write!(self.helpers, "{}", turned_written(turning));
                self.line(indent, &format!("turned{}(stdout)", turning.id));
            }
            Stmt::Left { id, depth, together, rounds, at, how } => {
                let _ = write!(self.helpers, "{}", left_written(*id, *depth, *together, *rounds, *at, *how));
                self.line(indent, &format!("left{id}(stdout) except why:"));
                self.line(indent + 1, "stdout.print(\"refused\", why.code)");
            }
            Stmt::FloatKeys { held, names: (keyed, seen), values, probe, asked, pair: (left, right) } => {
                let ty = match held {
                    FloatHeld::Gauge => "Gauge",
                    FloatHeld::Gauge32 => "Gauge32",
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
                self.handled(
                    indent,
                    &format!("sent{id}({name}, items={items})"),
                    &["Crashed", "Closed", "Timeout"],
                    "stdout.print(\"crashed\")",
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
        FloatHeld::Gauge32 => format!("Gauge32(at=f32({float}))"),
        FloatHeld::Level if float == "Off" => format!("{}.Off", enum_named("Level")),
        FloatHeld::Level => format!("{}.At(value={float})", enum_named("Level")),
        FloatHeld::Level32 if float == "Off" => format!("{}.Off", enum_named("Level32")),
        FloatHeld::Level32 => format!("{}.At(value=f32({float}))", enum_named("Level32")),
    }
}
