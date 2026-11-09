//! Writing a program tree from bytes. Each byte chooses something, so fewer bytes make a smaller
//! program, and cutting a failing input down cuts the program down. Every loop is bounded, so
//! every program ends, and every name is printed when its block ends, which is the digest the
//! builds and the evaluator are compared on.
//!
//! Most programs keep their numbers small and their indexes in range, so they run to the end. One
//! in eight is risky: big numbers, indexes as they come and division by anything, to compare where
//! and whether the builds and the evaluator fail.

use super::ast::{Arith, Case, Compare, Expr, Place, Program, Stmt, TYPES, Ty};

/// How deep expressions nest.
const MOST_DEPTH: usize = 3;
/// How deep blocks nest.
const MOST_BLOCKS: usize = 3;
/// The most statements a block has.
const MOST_STATEMENTS: u8 = 6;

struct Writer<'b> {
    bytes: std::slice::Iter<'b, u8>,
    /// The names in scope: each one's type, whether it can change, and whether it can't for now,
    /// as the list a loop goes over can't.
    names: Vec<InScope>,
    made: usize,
    risky: bool,
    /// How many loops the statement being written is inside.
    loops: usize,
}

/// A name in scope.
#[derive(Clone)]
struct InScope {
    name: String,
    ty: Ty,
    changeable: bool,
    /// How many loops over it are running, which it can't change under.
    held: usize,
}

/// The program these bytes choose.
pub(crate) fn program(bytes: &[u8]) -> Program {
    let risky = bytes.first().is_some_and(|first| first % 8 == 0);
    let bytes = bytes.get(1..).unwrap_or_default();
    let mut writer = Writer { bytes: bytes.iter(), names: Vec::new(), made: 0, risky, loops: 0 };
    let mut body = Vec::new();
    while writer.bytes.len() > 0 && body.len() < 64 {
        writer.statement(&mut body, 0);
    }
    writer.print_from(&mut body, 0);
    Program { body }
}

impl Writer<'_> {
    /// The next choice. Running out always chooses 0, so every input ends.
    fn byte(&mut self) -> u8 {
        self.bytes.next().copied().unwrap_or(0)
    }

    fn choice(&mut self, count: usize) -> usize {
        usize::from(self.byte()) % count.max(1)
    }

    fn fresh(&mut self) -> String {
        self.made += 1;
        format!("v{}", self.made)
    }

    /// A name of type `ty` in scope, one that can change now if `changing` says so.
    fn name_of(&mut self, ty: Ty, changing: bool) -> Option<String> {
        let fits: Vec<String> = self
            .names
            .iter()
            .filter(|name| name.ty == ty && (!changing || (name.changeable && name.held == 0)))
            .map(|name| name.name.clone())
            .collect();
        let at = self.choice(fits.len());
        fits.get(at).cloned()
    }

    fn small_int(&mut self) -> Expr {
        const SMALL: [i64; 8] = [0, 1, 2, 3, -1, 7, 100, -5];
        const BIG: [i64; 4] = [i64::MAX, i64::MIN + 1, 4_611_686_018_427_387_904, -4_611_686_018_427_387_904];
        if self.risky && self.choice(4) == 0 {
            return Expr::Int(BIG.get(self.choice(BIG.len())).copied().unwrap_or(0));
        }
        Expr::Int(SMALL.get(self.choice(SMALL.len())).copied().unwrap_or(0))
    }

    /// An int that stays small where the program isn't risky.
    fn bounded(&self, value: Expr) -> Expr {
        if self.risky { value } else { Expr::Small(Box::new(value)) }
    }

    fn int(&mut self, depth: usize) -> Expr {
        if depth >= MOST_DEPTH {
            return self.name_of(Ty::Int, false).map_or_else(|| self.small_int(), Expr::Name);
        }
        let next = depth + 1;
        match self.choice(17) {
            14 => Expr::IntOf(Box::new(self.of(Ty::Byte, next))),
            15 => {
                let limit = if self.risky {
                    Expr::Arith(Arith::Rem, Box::new(self.int(next)), Box::new(Expr::Int(9)))
                } else {
                    Expr::Int(i64::from(self.byte() % 8))
                };
                Expr::SumCounted(Box::new(limit))
            }
            16 => self.name_of(Ty::Tally, false).map_or_else(|| self.small_int(), Expr::Count),
            0 | 1 => self.small_int(),
            2 | 3 => self.name_of(Ty::Int, false).map_or_else(|| self.small_int(), Expr::Name),
            4 => {
                let operator =
                    [Arith::Add, Arith::Sub, Arith::Mul].get(self.choice(3)).copied().unwrap_or(Arith::Add);
                let (left, right) = (self.int(next), self.int(next));
                self.bounded(Expr::Arith(operator, Box::new(left), Box::new(right)))
            }
            5 => {
                let operator = if self.choice(2) == 0 { Arith::Div } else { Arith::Rem };
                let left = self.int(next);
                let right =
                    if self.risky { self.int(next) } else { Expr::Int(1 + i64::from(self.byte() % 9)) };
                Expr::Arith(operator, Box::new(left), Box::new(right))
            }
            6 => Expr::Len(Box::new(self.of(Ty::Ints, next))),
            7 if self.risky => Expr::Index(Box::new(self.of(Ty::Ints, next)), Box::new(self.int(next))),
            7 => Expr::AtOrZero(Box::new(self.of(Ty::Ints, next)), Box::new(self.int(next))),
            8 => {
                let sum = Expr::Sum(Box::new(self.of(Ty::Ints, next)));
                self.bounded(sum)
            }
            9 => {
                let point = self.of(Ty::Point, next);
                if self.choice(2) == 0 {
                    self.bounded(Expr::Total(Box::new(point)))
                } else {
                    Expr::Field(Box::new(point), self.choice(2) == 0)
                }
            }
            10 => {
                let area = Expr::Area(Box::new(self.of(Ty::Shape, next)));
                self.bounded(area)
            }
            11 => Expr::TextLen(Box::new(self.of(Ty::Str, next))),
            // Only at the top of an expression, never inside a comprehension, which can't change
            // the names around it.
            12 if depth == 0 => match self.name_of(Ty::Int, true) {
                Some(name) => {
                    let by = self.small_int();
                    let bumped = Expr::Bump(name, Box::new(by));
                    self.bounded(bumped)
                }
                None => self.small_int(),
            },
            _ => {
                let (when, yes, no) = (self.of(Ty::Bool, next), self.int(next), self.int(next));
                Expr::Choose(Box::new(when), Box::new(yes), Box::new(no))
            }
        }
    }

    /// A value of `ty`: a name that holds one, now and then, or one made here.
    fn of(&mut self, ty: Ty, depth: usize) -> Expr {
        if ty != Ty::Int
            && let Some(name) = self.name_of(ty, false).filter(|_| depth >= MOST_DEPTH || self.choice(2) == 0)
        {
            return Expr::Name(name);
        }
        let next = depth + 1;
        match ty {
            Ty::Int => self.int(depth),
            Ty::Bool if depth >= MOST_DEPTH => Expr::Bool(self.choice(2) == 0),
            Ty::Bool => match self.choice(4) {
                0 => Expr::Bool(self.choice(2) == 0),
                1 => Expr::And(Box::new(self.of(Ty::Bool, next)), Box::new(self.of(Ty::Bool, next))),
                2 => Expr::Not(Box::new(self.of(Ty::Bool, next))),
                _ => {
                    const COMPARES: [Compare; 5] =
                        [Compare::Less, Compare::LessEq, Compare::Equal, Compare::NotEqual, Compare::Greater];
                    let compare = COMPARES.get(self.choice(COMPARES.len())).copied().unwrap_or(Compare::Less);
                    Expr::Compare(compare, Box::new(self.int(next)), Box::new(self.int(next)))
                }
            },
            Ty::Str if depth >= MOST_DEPTH => Expr::Str("fox".to_owned()),
            Ty::Str => match self.choice(4) {
                // A name or a number, since an f-string can't hold another string.
                0 => Expr::Format(Box::new(self.int(MOST_DEPTH))),
                // A text and something new, never a name with itself, which doubled in a loop
                // holds more than any machine has.
                1 => {
                    let tail = if self.choice(2) == 0 {
                        Expr::Str("ab".to_owned())
                    } else {
                        Expr::Format(Box::new(self.int(MOST_DEPTH)))
                    };
                    Expr::Concat(Box::new(self.of(Ty::Str, next)), Box::new(tail))
                }
                2 => Expr::Str("hé🦊".to_owned()),
                _ => Expr::Str(String::new()),
            },
            Ty::Ints if depth >= MOST_DEPTH => Expr::Ints(vec![Expr::Int(1), Expr::Int(2)]),
            Ty::Ints => match self.choice(4) {
                0 => {
                    let count = self.choice(4);
                    Expr::Ints((0..count).map(|_| self.int(next)).collect())
                }
                1 => {
                    let list = self.of(Ty::Ints, next);
                    let by = if self.risky { self.int(next) } else { Expr::Int(i64::from(self.byte() % 4)) };
                    let plus = self.int(MOST_DEPTH);
                    let plus = self.bounded(plus);
                    Expr::Mapped(Box::new(list), Box::new(by), Box::new(plus))
                }
                2 => Expr::Ints(Vec::new()),
                _ => Expr::Ints(vec![Expr::Int(3), Expr::Int(1), Expr::Int(2)]),
            },
            Ty::Point => Expr::Point(Box::new(self.int(next)), Box::new(self.int(next))),
            Ty::Byte if depth >= MOST_DEPTH => Expr::Byte(Box::new(Expr::Int(i64::from(self.byte())))),
            Ty::Byte => match self.choice(4) {
                0 => {
                    let value = self.int(next);
                    // Kept in range where nothing's meant to fail. A literal is checked to fit when
                    // the program's compiled, so only what's worked out can be out of range.
                    let value = if let Expr::Int(n) = value {
                        Expr::Int(n.rem_euclid(256))
                    } else if self.risky {
                        value
                    } else {
                        Expr::Arith(Arith::Rem, Box::new(value), Box::new(Expr::Int(256)))
                    };
                    Expr::Byte(Box::new(value))
                }
                1 => Expr::ByteWrap(Box::new(self.of(Ty::Byte, next)), Box::new(self.of(Ty::Byte, next))),
                2 if self.risky => {
                    Expr::ByteAdd(Box::new(self.of(Ty::Byte, next)), Box::new(self.of(Ty::Byte, next)))
                }
                _ => Expr::Byte(Box::new(Expr::Int(i64::from(self.byte())))),
            },
            // These are made by statements of their own, never in an expression.
            Ty::Shared | Ty::Tally => Expr::Int(0),
            Ty::Shape => match self.choice(3) {
                0 => Expr::Shape(Case::Circle, Box::new(self.int(next))),
                1 => Expr::Shape(Case::Square, Box::new(self.int(next))),
                _ => Expr::Shape(Case::Empty, Box::new(Expr::Int(0))),
            },
        }
    }

    fn ty(&mut self) -> Ty {
        TYPES.get(self.choice(TYPES.len())).copied().unwrap_or(Ty::Int)
    }

    /// A block's statements, its names printed at its end and then forgotten.
    fn block(&mut self, depth: usize) -> Vec<Stmt> {
        let scope = self.names.len();
        let mut body = Vec::new();
        let count = 1 + self.byte() % MOST_STATEMENTS;
        for _ in 0..count {
            self.statement(&mut body, depth);
        }
        self.print_from(&mut body, scope);
        self.names.truncate(scope);
        body
    }

    /// Prints each name declared since `scope`, so the digest has every value, and so no change
    /// goes unread, which the checker refuses.
    fn print_from(&mut self, body: &mut Vec<Stmt>, scope: usize) {
        for name in self.names.iter().skip(scope).filter(|name| name.ty == Ty::Shared) {
            body.push(Stmt::PrintShared(name.name.clone()));
        }
        let values: Vec<(Expr, Ty)> = self
            .names
            .iter()
            .skip(scope)
            .filter(|name| !matches!(name.ty, Ty::Shared | Ty::Tally))
            .map(|name| (Expr::Name(name.name.clone()), name.ty))
            .collect();
        for chunk in values.chunks(4) {
            body.push(Stmt::Print(chunk.to_vec()));
        }
    }

    fn statement(&mut self, body: &mut Vec<Stmt>, depth: usize) {
        let which = self.choice(21);
        match which {
            3..=5 | 7 | 19 | 20 if depth < MOST_BLOCKS => self.nested(body, depth, which),
            16..=18 => self.sharing(body, which),
            0..=2 => {
                let ty = self.ty();
                let value = self.of(ty, 0);
                let name = self.fresh();
                let changeable = self.choice(2) == 0;
                body.push(Stmt::Let { name: name.clone(), value, changeable });
                self.names.push(InScope { name: name.clone(), ty, changeable, held: 0 });
                if changeable {
                    self.read_back(body, &name);
                }
            }
            6 => {
                let Some(list) = self.name_of(Ty::Ints, true) else { return self.print(body) };
                let item = self.fresh();
                let by = if self.risky { self.small_int() } else { Expr::Int(i64::from(self.byte() % 5)) };
                body.push(Stmt::EachRef(item, list.clone(), by));
                self.read_back(body, &list);
            }
            8 if self.loops > 0 => {
                let condition = self.of(Ty::Bool, 1);
                body.push(if self.choice(2) == 0 {
                    Stmt::Break(condition)
                } else {
                    Stmt::Continue(condition)
                });
            }
            9 => self.change_int(body),
            10 => {
                let Some(list) = self.name_of(Ty::Ints, true) else { return self.print(body) };
                match self.choice(3) {
                    0 => {
                        let value = self.int(1);
                        body.push(Stmt::Append(list.clone(), value));
                        self.read_back(body, &list);
                    }
                    1 => {
                        body.push(Stmt::Pop(list.clone()));
                        self.read_back(body, &list);
                    }
                    _ => self.set_item(body, &list),
                }
            }
            11 => {
                let Some(point) = self.name_of(Ty::Point, true) else { return self.print(body) };
                let by = self.int(1);
                let by = self.bounded(by);
                if self.choice(2) == 0 {
                    body.push(Stmt::Moved(point.clone(), by));
                } else {
                    let place = Place::Field(point.clone(), self.choice(2) == 0);
                    body.push(Stmt::Assign { place, value: by, add: self.choice(2) == 0 });
                }
                self.read_back(body, &point);
            }
            12 => {
                let Some(shape) = self.name_of(Ty::Shape, true) else { return self.print(body) };
                let by = self.int(1);
                let by = self.bounded(by);
                body.push(Stmt::Grow(shape.clone(), by));
                self.read_back(body, &shape);
            }
            13 => {
                let ty = self.ty();
                let Some(name) = self.name_of(ty, true) else { return self.print(body) };
                let value = self.of(ty, 1);
                body.push(Stmt::Assign { place: Place::Name(name.clone()), value, add: false });
                self.read_back(body, &name);
            }
            _ => self.print(body),
        }
    }

    /// A statement holding a block: `if`, the loops, and `with`.
    fn nested(&mut self, body: &mut Vec<Stmt>, depth: usize, which: usize) {
        match which {
            3 => {
                let condition = self.of(Ty::Bool, 0);
                let then = self.block(depth + 1);
                let otherwise = if self.choice(2) == 0 { Vec::new() } else { self.block(depth + 1) };
                body.push(Stmt::If(condition, then, otherwise));
            }
            4 => {
                let (name, count) = (self.fresh(), self.count(5));
                let inner = self.round(&name, Ty::Int, depth);
                body.push(Stmt::Range(name, count, inner));
            }
            5 => self.each(body, depth),
            7 => {
                let counter = self.fresh();
                let limit = i64::from(self.byte() % 6);
                // The counter is the loop's own, and the body doesn't change it.
                self.names.push(InScope { name: counter.clone(), ty: Ty::Int, changeable: true, held: 1 });
                self.loops += 1;
                let inner = self.block(depth + 1);
                self.loops -= 1;
                self.names.pop();
                body.push(Stmt::While(counter, limit, inner));
            }
            19 => {
                let (name, limit) = (self.fresh(), self.count(5));
                let inner = self.round(&name, Ty::Int, depth);
                body.push(Stmt::EachCounted(name, limit, inner));
            }
            _ => {
                let name = self.fresh();
                let start = self.int(1);
                self.names.push(InScope {
                    name: name.clone(),
                    ty: Ty::Tally,
                    changeable: false,
                    held: 0,
                });
                let inner = self.block(depth + 1);
                self.names.pop();
                body.push(Stmt::With(name, start, inner));
            }
        }
    }

    /// How many rounds a loop goes: fewer than `most`, or in a risky program what's worked out,
    /// kept below 6, since past a few rounds a loop only takes longer.
    fn count(&mut self, most: u8) -> Expr {
        if self.risky {
            Expr::Arith(Arith::Rem, Box::new(self.int(1)), Box::new(Expr::Int(6)))
        } else {
            Expr::Int(i64::from(self.byte() % most))
        }
    }

    /// A loop's body, with its item `name` in scope.
    fn round(&mut self, name: &str, ty: Ty, depth: usize) -> Vec<Stmt> {
        self.names.push(InScope { name: name.to_owned(), ty, changeable: false, held: 0 });
        self.loops += 1;
        let inner = self.block(depth + 1);
        self.loops -= 1;
        self.names.pop();
        inner
    }

    /// `for item in list:`, with the list held so the body can't change it.
    fn each(&mut self, body: &mut Vec<Stmt>, depth: usize) {
        let list = self.of(Ty::Ints, 1);
        let looped = if let Expr::Name(looped) = &list { Some(looped.clone()) } else { None };
        let held: Vec<usize> = self
            .names
            .iter()
            .enumerate()
            .filter(|(_, name)| Some(&name.name) == looped.as_ref())
            .map(|(at, _)| at)
            .collect();
        for at in &held {
            if let Some(name) = self.names.get_mut(*at) {
                name.held += 1;
            }
        }
        let item = self.fresh();
        let inner = self.round(&item, Ty::Int, depth);
        for at in held {
            if let Some(name) = self.names.get_mut(at) {
                name.held -= 1;
            }
        }
        body.push(Stmt::Each(item, list, inner));
    }

    /// A `shared Point`, or a change to one under its lock.
    fn sharing(&mut self, body: &mut Vec<Stmt>, which: usize) {
        if which == 16 {
            let name = self.fresh();
            let (x, y) = (self.int(1), self.int(1));
            body.push(Stmt::Share(name.clone(), x, y));
            self.names.push(InScope { name, ty: Ty::Shared, changeable: false, held: 0 });
            return;
        }
        let Some(shared) = self.name_of(Ty::Shared, false) else { return self.print(body) };
        if which == 18 {
            let items = self.of(Ty::Ints, 1);
            body.push(Stmt::Tally(shared, items));
            return;
        }
        let by = self.int(1);
        let by = self.bounded(by);
        let refuse = self.of(Ty::Bool, 1);
        body.push(Stmt::Change(shared, by, refuse));
    }

    fn change_int(&mut self, body: &mut Vec<Stmt>) {
        let Some(name) = self.name_of(Ty::Int, true) else { return self.print(body) };
        let value = self.int(0);
        let value = self.bounded(value);
        body.push(Stmt::Assign { place: Place::Name(name.clone()), value, add: self.choice(2) == 0 });
        self.read_back(body, &name);
    }

    /// `list[at] = value` or `+=`, with `at` kept in the list unless the program is risky.
    fn set_item(&mut self, body: &mut Vec<Stmt>, list: &str) {
        let at = self.int(2);
        let value = self.int(0);
        let value = self.bounded(value);
        let add = self.choice(2) == 0;
        if self.risky {
            body.push(Stmt::Assign { place: Place::Item(list.to_owned(), at), value, add });
            self.read_back(body, list);
            return;
        }
        let length = Expr::Len(Box::new(Expr::Name(list.to_owned())));
        let inside = Expr::Arith(Arith::Rem, Box::new(at), Box::new(length.clone()));
        let assign = Stmt::Assign { place: Place::Item(list.to_owned(), inside), value, add };
        let any = Expr::Compare(Compare::Greater, Box::new(length), Box::new(Expr::Int(0)));
        body.push(Stmt::If(any, vec![assign], Vec::new()));
        self.read_back(body, list);
    }

    /// Prints a name right after it's written, so no write goes unread, which the checker
    /// refuses, and every write is in the digest.
    fn read_back(&self, body: &mut Vec<Stmt>, name: &str) {
        if let Some(found) = self.names.iter().rev().find(|held| held.name == name) {
            body.push(Stmt::Print(vec![(Expr::Name(found.name.clone()), found.ty)]));
        }
    }

    fn print(&mut self, body: &mut Vec<Stmt>) {
        let ty = self.ty();
        let value = self.of(ty, 0);
        body.push(Stmt::Print(vec![(value, ty)]));
    }
}
