//! Writing valid programs from bytes, to run on both engines. Each byte chooses something, so
//! fewer bytes make a smaller program, and cutting a failing input down cuts down the program.
//! Every loop is bounded, so every program ends.
//!
//! Most programs are written so that nothing fails, since a run that stops at its first overflow
//! tests little: numbers stay small and indexes stay in range. One in eight isn't, to compare how
//! the engines fail.

use std::fmt::Write as _;

/// How deep statements and expressions nest.
const MOST_DEPTH: usize = 3;
/// The most statements a block has.
const MOST_STATEMENTS: u8 = 6;

/// The kinds of values programs hold.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Float,
    Bool,
    Str,
    Ints,
    Scores,
    Point,
    Shape,
}

const KINDS: [Kind; 8] =
    [Kind::Int, Kind::Float, Kind::Bool, Kind::Str, Kind::Ints, Kind::Scores, Kind::Point, Kind::Shape];

/// What every program starts with, for the rest to use.
const HEADER: &str = "struct Point:
    x: int
    y: int

    def total(self) -> int:
        self.x + self.y

    def moved(mut self, by: int):
        self.x += by

enum Shape:
    Circle(radius: int)
    Square(side: int)
    Empty

error TooBig:
    limit: int

def area(shape: Shape) -> int:
    match shape:
        case .Circle(radius): radius * radius * 3
        case .Square(side): side * side
        case .Empty: 0

def small(n: int) -> int:
    if n > 1000: fail TooBig(limit=1000)
    n + 1

def total_of(items: list[int]) -> int:
    mut total = 0
    for item in items:
        total += item
    total

def twice[T](value: T) -> list[T]:
    [value, value]

def at_or_zero(items: list[int], at: int) -> int:
    if items.len() == 0: return 0
    items[at % items.len()]

";

struct Writer<'b> {
    bytes: std::slice::Iter<'b, u8>,
    source: String,
    /// The names in scope: what kind each holds, and whether it can change.
    names: Vec<(String, Kind, bool)>,
    made: usize,
    /// Whether what's written may fail while it runs.
    risky: bool,
}

impl Writer<'_> {
    /// The next choice. Running out always chooses 0, so every input ends.
    fn byte(&mut self) -> u8 {
        self.bytes.next().copied().unwrap_or(0)
    }

    fn choice(&mut self, count: usize) -> usize {
        usize::from(self.byte()) % count.max(1)
    }

    /// One of some words.
    fn word<'w>(&mut self, from: &[&'w str]) -> &'w str {
        from.get(self.choice(from.len())).copied().unwrap_or("")
    }

    fn kind(&mut self) -> Kind {
        KINDS.get(self.choice(KINDS.len())).copied().unwrap_or(Kind::Int)
    }

    fn name_of(&mut self, kind: Kind, changeable: bool) -> Option<String> {
        let fits: Vec<&String> = self
            .names
            .iter()
            .filter(|(_, held, can_change)| *held == kind && (*can_change || !changeable))
            .map(|(name, ..)| name)
            .collect();
        let chosen = usize::from(self.bytes.next().copied().unwrap_or(0)) % fits.len().max(1);
        fits.get(chosen).map(|name| (*name).clone())
    }

    fn int(&mut self, depth: usize) -> String {
        if depth >= MOST_DEPTH {
            return self.small_int();
        }
        let next = depth + 1;
        match self.choice(12) {
            0..=2 => self.small_int(),
            3 => self.name_of(Kind::Int, false).unwrap_or_else(|| "2".to_owned()),
            4 => {
                let operator = self.word(&["+", "-", "*"]);
                {
                    let (left, right) = (self.int(next), self.int(next));
                    self.bounded(&format!("{left} {operator} {right}"))
                }
            }
            5 if self.risky => {
                format!("({} {} {})", self.int(next), self.word(&["//", "%"]), self.int(next))
            }
            5 => format!("({} {} {})", self.int(next), self.word(&["//", "%"]), 1 + self.choice(9)),
            6 => format!("{}.len()", self.of(Kind::Ints, next)),
            7 if self.risky => format!("{}[{}]", self.of(Kind::Ints, next), self.int(next)),
            7 => format!("at_or_zero({}, {})", self.of(Kind::Ints, next), self.int(next)),
            8 => {
                let items = self.of(Kind::Ints, next);
                self.bounded(&format!("total_of({items})"))
            }
            9 => {
                let point = self.of(Kind::Point, next);
                self.bounded(&format!("{point}.total()"))
            }
            10 => {
                let shape = self.of(Kind::Shape, next);
                self.bounded(&format!("area({shape})"))
            }
            _ => format!("({} if {} else {})", self.int(next), self.of(Kind::Bool, next), self.int(next)),
        }
    }

    /// An int kept small, unless this program may fail.
    fn bounded(&self, value: &str) -> String {
        if self.risky { format!("({value})") } else { format!("(({value}) % 1009)") }
    }

    fn small_int(&mut self) -> String {
        const SMALL: [&str; 7] = ["0", "1", "2", "3", "-1", "7", "100"];
        const BIG: [&str; 3] = ["9223372036854775807", "4611686018427387904", "-9223372036854775807"];
        if self.risky && self.choice(4) == 0 {
            return self.word(&BIG).to_owned();
        }
        self.word(&SMALL).to_owned()
    }

    fn of(&mut self, kind: Kind, depth: usize) -> String {
        if let Some(name) = self.name_of(kind, false).filter(|_| depth >= MOST_DEPTH || self.choice(2) == 0) {
            return name;
        }
        let next = depth + 1;
        match kind {
            Kind::Int => self.int(depth),
            Kind::Float => self.word(&["0.5", "1.5", "-2.25", "1e10"]).to_owned(),
            Kind::Bool if depth >= MOST_DEPTH => "True".to_owned(),
            Kind::Bool => {
                let compare = self.word(&["<", "<=", "==", "!=", ">"]);
                format!("({} {compare} {})", self.int(next), self.int(next))
            }
            Kind::Str if depth >= MOST_DEPTH => "\"a\"".to_owned(),
            Kind::Str => match self.choice(4) {
                0 => format!("f\"{{{}}}\"", self.int(next)),
                1 => format!("({} + {})", self.of(Kind::Str, next), self.of(Kind::Str, next)),
                2 => "\"hé🦊\"".to_owned(),
                _ => "\"fox\"".to_owned(),
            },
            Kind::Ints if depth >= MOST_DEPTH => "[1, 2]".to_owned(),
            Kind::Ints => match self.choice(4) {
                0 => format!("[{}, {}]", self.int(next), self.int(next)),
                1 => format!("twice({})", self.int(next)),
                2 => format!("[item + 1 for item in {}]", self.of(Kind::Ints, next)),
                _ => "[3, 1, 2]".to_owned(),
            },
            Kind::Scores => format!("{{\"a\": {}, \"b\": {}}}", self.int(next), self.int(next)),
            Kind::Point => format!("Point(x={}, y={})", self.int(next), self.int(next)),
            Kind::Shape => match self.choice(3) {
                0 => format!("Shape.Circle({})", self.int(next)),
                1 => format!("Shape.Square({})", self.int(next)),
                _ => "Shape.Empty".to_owned(),
            },
        }
    }

    fn line(&mut self, indent: usize, text: &str) {
        let _ = writeln!(self.source, "{}{text}", "    ".repeat(indent));
    }

    fn fresh(&mut self) -> String {
        self.made += 1;
        format!("v{}", self.made)
    }

    fn block(&mut self, indent: usize) {
        let scope = self.names.len();
        let count = 1 + self.byte() % MOST_STATEMENTS;
        for _ in 0..count {
            self.statement(indent);
        }
        self.names.truncate(scope);
    }

    fn statement(&mut self, indent: usize) {
        let deep = indent > MOST_DEPTH;
        match self.choice(11) {
            0 | 1 => {
                let kind = self.kind();
                let value = self.of(kind, 0);
                let name = self.fresh();
                let changeable = self.choice(2) == 0;
                let word = if changeable { "mut " } else { "" };
                self.line(indent, &format!("{word}{name} = {value}"));
                self.names.push((name, kind, changeable));
            }
            2 if !deep => {
                let condition = self.of(Kind::Bool, 0);
                self.line(indent, &format!("if {condition}:"));
                self.block(indent + 1);
                self.line(indent, "else:");
                self.block(indent + 1);
            }
            3 if !deep => {
                let (name, times) = (self.fresh(), self.choice(5));
                self.line(indent, &format!("for {name} in range({times}):"));
                self.names.push((name, Kind::Int, false));
                self.block(indent + 1);
                self.names.pop();
            }
            4 => self.change_int(indent),
            5 => {
                let Some(list) = self.name_of(Kind::Ints, true) else { return self.print(indent) };
                let value = self.int(1);
                let change = match self.choice(3) {
                    0 => format!("{list}.append({value})"),
                    1 if self.risky => format!("{list}[{}] = {value}", self.int(2)),
                    1 => format!("if {list}.len() > 0: {list}[{} % {list}.len()] = {value}", self.int(2)),
                    _ if self.risky => format!("{list}.pop()"),
                    _ => format!("if {list}.len() > 0: {list}.pop()"),
                };
                self.line(indent, &change);
            }
            6 => {
                let Some(scores) = self.name_of(Kind::Scores, true) else { return self.print(indent) };
                let key = self.of(Kind::Str, 2);
                let value = self.int(1);
                self.line(indent, &format!("{scores}[{key}] = {value}"));
            }
            7 => {
                let Some(point) = self.name_of(Kind::Point, true) else { return self.print(indent) };
                let by = self.int(1);
                let change = if self.choice(2) == 0 {
                    format!("{point}.moved({by})")
                } else {
                    format!("{point}.y = {by}")
                };
                self.line(indent, &change);
            }
            _ => self.print(indent),
        }
    }

    fn change_int(&mut self, indent: usize) {
        let Some(name) = self.name_of(Kind::Int, true) else { return self.print(indent) };
        let by = self.int(1);
        let operator = self.word(&["+", "-", "*"]);
        let value = self.bounded(&format!("{name} {operator} {by}"));
        self.line(indent, &format!("{name} = {value}"));
    }

    fn print(&mut self, indent: usize) {
        let kind = self.kind();
        let value = self.of(kind, 0);
        self.line(indent, &format!("stdout.print({value})"));
    }
}

/// The program these bytes choose.
pub(crate) fn program(bytes: &[u8]) -> String {
    let risky = bytes.first().is_some_and(|first| first % 8 == 0);
    let bytes = bytes.get(1..).unwrap_or_default();
    let mut writer =
        Writer { bytes: bytes.iter(), source: HEADER.to_owned(), names: Vec::new(), made: 0, risky };
    writer.line(0, "def main(stdout: Stdout):");
    while writer.bytes.len() > 0 && writer.source.len() < 1 << 14 {
        writer.statement(1);
    }
    writer.line(1, "stdout.print(\"done\")");
    writer.source
}

#[cfg(test)]
mod tests {
    use super::program;

    #[test]
    fn the_same_bytes_write_the_same_program() {
        assert_eq!(program(b"some bytes"), program(b"some bytes"));
        assert_ne!(program(b"some bytes"), program(b"other bytes!"));
    }

    #[test]
    fn no_bytes_is_a_program_too() {
        assert!(fennec_check::check(&fennec_syntax::parse(&program(b"")).unwrap()).is_ok());
    }

    #[test]
    fn what_it_writes_is_a_program_the_checker_accepts() {
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..64).map(|at| at.wrapping_mul(seed).wrapping_add(seed)).collect();
            let source = program(&bytes);
            let module = fennec_syntax::parse(&source).unwrap();
            let checked = fennec_check::check(&module);
            assert!(checked.is_ok(), "{source}\n{:?}", checked.err());
        }
    }
}
