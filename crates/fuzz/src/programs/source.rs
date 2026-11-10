//! A program tree written out as Fennec.

#![expect(clippy::too_many_lines, reason = "one match writes each statement, and one each expression")]

use std::fmt::Write as _;

use super::ast::{Amount, Arith, BufOp, Case, Compare, Expr, Op, Place, Program, RefPlace, Stmt};

/// What every program starts with, for its body to use.
const HEADER: &str = "from std.collections import Table
from std.concurrent import Closed, Crashed, Inbox, Timeout

struct Point:
    x: int
    y: int

    def total(self) -> int:
        self.x + self.y

    def moved(ref self, by: int):
        self.x += by

enum Shape:
    Circle(radius: int)
    Square(side: int)
    Empty

def area(shape: Shape) -> int:
    match shape:
        case .Circle(radius): radius * radius * 3
        case .Square(side): side * side
        case .Empty: 0

def at_or_zero(items: list[int], at: int) -> int:
    if items.len() == 0: return 0
    items[at % items.len()]

def bump(ref value: int, by: int) -> int:
    value += by
    value * 10

error Refused:
    code: int

def change(box: shared Point, by: int, refuse: bool) fails Refused:
    lock box as ref held:
        held.x += by
        held.y = held.y * 2
        if refuse:
            fail Refused(code=by)

def tallying(inbox: Inbox[int], totals: shared Point):
    for number in inbox:
        lock totals as ref held:
            held.y += 1
            held.x += number

def tally(totals: shared Point, items: list[int]) fails Crashed | Closed | Timeout:
    with launch((inbox) => tallying(inbox, totals))? as sends:
        for item in items:
            sends.send(item)?

def grow_all(ref items: list[int], by: int):
    for ref item in items:
        item += by

def buffer_of(made: Buffer[int], items: list[int]) -> Buffer[int]:
    mut buffer = made
    for item in items:
        buffer.push(item)
    buffer

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
    lend Tally(count=code) else:
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
    total

struct Holder:
    items: list[int]
    tag: int

enum Node:
    Leaf(name: str)
    Number(value: int)
    Branch(label: str, children: list[Node])
    Tagged(tags: list[str], inner: list[Node])

def deep(depth: int, label: str) -> Node:
    mut node = Node.Leaf(name=label)
    for _ in range(depth):
        node = .Branch(label=label, children=[node])
    node

def depth_of(node: Node) -> int:
    mut at = node
    mut depth = 0
    while True:
        match at:
            case .Branch(_, children):
                if children.len() == 0:
                    return depth
                at = children[0]
                depth += 1
            case _:
                return depth
    depth

def digest(node: Node) -> str:
    match node:
        case .Leaf(name):
            \"L(\" + name + \")\"
        case .Number(value):
            f\"N({value})\"
        case .Branch(label, children):
            mut text = \"B(\" + label
            for child in children:
                text += \" \" + digest(child)
            text + \")\"
        case .Tagged(tags, inner):
            mut text = \"T(\"
            for tag in tags:
                text += tag + \",\"
            for child in inner:
                text += \" \" + digest(child)
            text + \")\"

def digest_all(nodes: list[Node]) -> str:
    mut text = \"[\"
    for node in nodes:
        text += digest(node) + \";\"
    text + \"]\"

def node_or_leaf(nodes: list[Node], at: int) -> Node:
    if nodes.len() == 0: return .Leaf(name=\"none\")
    nodes[at % nodes.len()]

def tallied(start: int, stdout: Stdout) -> with Tally:
    stdout.print(\"open\", start)
    lend Tally(count=start) else:
        stdout.print(\"undone\", start)
    stdout.print(\"closed\", start)

";

/// The program as Fennec source.
pub(crate) fn source(program: &Program) -> String {
    let mut writer = Writer { out: String::new(), helpers: String::new(), made: 0 };
    writer.block(&program.body, 1);
    writer.line(1, "stdout.print(\"done\")");
    format!("{HEADER}{}def main(stdout: Stdout):\n{}", writer.helpers, writer.out)
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
                format!("ref slot{number} = held.items[{at}]\n{pad}slot{number} += {}", amount(*value))
            }
        };
        let _ = writeln!(out, "{pad}{line}");
    }
    out
}

impl Writer {
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
                let written = match value {
                    // A second changeable name for a value is a copy, said so.
                    Expr::Name(other) if *changeable => format!("copy({other})"),
                    _ => self.expr(value, false),
                };
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
                self.line(indent, &format!("for {name} in range({list}.len()):"));
                self.line(indent + 1, &format!("stdout.print({list}[{name}])"));
                self.block(body, indent + 1);
            }
            Stmt::SetKey(name, key, value, add) => {
                let value = self.expr(value, false);
                let operator = if *add { "+=" } else { "=" };
                self.line(indent, &format!("{name}[\"{key}\"] {operator} {value}"));
            }
            Stmt::RemoveKey(name, key) => self.line(indent, &format!("_ = {name}.remove(\"{key}\")")),
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
                self.line(indent, &format!("if {name}.len() > 0: {name}[{at} % {name}.len()] = {node}"));
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
                self.line(indent, &format!("match change({name}, by={by}, refuse={refuse}):"));
                self.line(indent + 1, "case Ok(_):");
                self.line(indent + 2, "pass");
                self.line(indent + 1, "case Err(_):");
                self.line(indent + 2, "stdout.print(\"refused\")");
            }
            Stmt::Tally(name, items) => {
                let items = self.expr(items, false);
                self.line(indent, &format!("match tally({name}, items={items}):"));
                self.line(indent + 1, "case Ok(_):");
                self.line(indent + 2, "pass");
                self.line(indent + 1, "case Err(_):");
                self.line(indent + 2, "stdout.print(\"crashed\")");
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
                self.line(indent, &format!("match bump_outer({name}, by={by}, refuse={refuse}):"));
                self.line(indent + 1, "case Ok(_):");
                self.line(indent + 2, "pass");
                self.line(indent + 1, "case Err(_):");
                self.line(indent + 2, "stdout.print(\"refused\")");
            }
            Stmt::Summed(limit, stop) => {
                let (limit, stop) = (self.expr(limit, false), self.expr(stop, false));
                self.line(indent, &format!("match summed_until({limit}, stop={stop}, stdout=stdout):"));
                self.line(indent + 1, "case Ok(total):");
                self.line(indent + 2, "stdout.print(total)");
                self.line(indent + 1, "case Err(_):");
                self.line(indent + 2, "stdout.print(\"shut\")");
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
                self.line(indent, &format!("match locked{id}({name}, refuse={refuse}):"));
                self.line(indent + 1, "case Ok(_):");
                self.line(indent + 2, "pass");
                self.line(indent + 1, "case Err(_):");
                self.line(indent + 2, "stdout.print(\"refused\")");
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
                self.line(indent, &format!("match sent{id}({name}, items={items}):"));
                self.line(indent + 1, "case Ok(_):");
                self.line(indent + 2, "pass");
                self.line(indent + 1, "case Err(_):");
                self.line(indent + 2, "stdout.print(\"crashed\")");
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
            Expr::Copy(name) => format!("copy({name})"),
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
            Expr::TextLen(text) => format!("{}.len()", self.expr(text, false)),
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
            Expr::ParallelSum(list, by) => {
                self.made += 1;
                let item = format!("item{}", self.made);
                let (by, list) = (self.expr(by, false), self.expr(list, false));
                format!("sum({item} * {by} for {item} in parallel({list}))")
            }
            Expr::Len(list) => format!("{}.len()", self.expr(list, false)),
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
            Expr::BufLen(buffer) => format!("{}.len()", self.expr(buffer, false)),
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
