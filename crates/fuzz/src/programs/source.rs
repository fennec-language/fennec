//! A program tree written out as Fennec.

use std::fmt::Write as _;

use super::ast::{Arith, Case, Compare, Expr, Place, Program, Stmt};

/// What every program starts with, for its body to use.
const HEADER: &str = "from std.concurrent import Closed, Crashed, Inbox, Timeout

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

def counted(limit: int) -> Iterator[int]:
    for at in range(limit):
        yield at * 3

struct Tally:
    count: int

def tallied(start: int, stdout: Stdout) -> with Tally:
    stdout.print(\"open\", start)
    lend Tally(count=start) else:
        stdout.print(\"undone\", start)
    stdout.print(\"closed\", start)

";

/// The program as Fennec source.
pub(crate) fn source(program: &Program) -> String {
    let mut writer = Writer { out: HEADER.to_owned(), made: 0 };
    writer.out.push_str("def main(stdout: Stdout):\n");
    writer.block(&program.body, 1);
    writer.line(1, "stdout.print(\"done\")");
    writer.out
}

struct Writer {
    out: String,
    /// How many names this writer has made up, for comprehensions.
    made: usize,
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
            Stmt::Grow(..) => self.held_or_lent(stmt, indent),
            Stmt::Share(..)
            | Stmt::Change(..)
            | Stmt::Tally(..)
            | Stmt::PrintShared(_)
            | Stmt::EachCounted(..)
            | Stmt::With(..) => {
                self.held_or_lent(stmt, indent);
            }
            Stmt::Print(values) => {
                let values: Vec<String> = values.iter().map(|(value, _)| self.expr(value, false)).collect();
                self.line(indent, &format!("stdout.print({})", values.join(", ")));
            }
        }
    }

    /// The statements about sharing, generators, `with` and `match`: what `stmt` writes for them.
    fn held_or_lent(&mut self, stmt: &Stmt, indent: usize) {
        match stmt {
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
            Expr::Compare(compare, left, right) => {
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
            Expr::Concat(left, right) | Expr::ByteAdd(left, right) => {
                format!("({} + {})", self.expr(left, false), self.expr(right, false))
            }
            Expr::Format(value) => format!("f\"<{{{}}}>\"", self.expr(value, false)),
            Expr::TextLen(text) => format!("{}.len()", self.expr(text, false)),
            // Reported: `list[int]()` where nothing else says the type fails to build once trimmed.
            // Take this out once the checker lane's `typed_empty_list` test passes.
            Expr::Ints(items) if items.is_empty() => "[0][0:0]".to_owned(),
            Expr::Ints(items) => {
                let items: Vec<String> = items.iter().map(|item| self.expr(item, false)).collect();
                format!("[{}]", items.join(", "))
            }
            Expr::Mapped(list, by, plus) => {
                self.made += 1;
                let item = format!("item{}", self.made);
                let (by, plus, list) = (self.expr(by, false), self.expr(plus, false), self.expr(list, false));
                format!("[{item} * {by} + {plus} for {item} in {list}]")
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
            Expr::Byte(value) => format!("u8({})", self.expr(value, false)),
            Expr::ByteWrap(left, right) => {
                format!("{}.wrapping_add({})", self.expr(left, false), self.expr(right, false))
            }
            Expr::IntOf(byte) => format!("int({})", self.expr(byte, false)),
            Expr::SumCounted(limit) => format!("sum(counted({}))", self.expr(limit, false)),
            Expr::Count(name) => format!("{name}.count"),
        }
    }
}
