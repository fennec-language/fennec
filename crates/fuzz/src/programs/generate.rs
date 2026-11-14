//! Writing a program tree from bytes. Each byte chooses something, and once they run out a
//! stream seeded by them goes on choosing, so the input decides the whole program and a short
//! one still makes a varied one. Every loop is bounded, so every program ends, and every name is
//! printed when it's written and when its block ends, which is the digest the builds and the
//! evaluator are compared on.
//!
//! Most programs keep their numbers small and their indexes in range, so they run to the end. One
//! in eight is risky: big numbers, indexes as they come and division by anything, to compare where
//! and whether the builds and the evaluator fail.

#![expect(
    clippy::too_many_lines,
    reason = "one match chooses each kind of statement, and one each kind of value"
)]

use super::ast::Reading;
use super::ast::{
    Alias, Aliasing, Amount, Arith, Arrow, BufOp, Case, Compare, Constants, Expr, FLOATS, FloatHeld, Held,
    Leave, Nested, Op, Place, Program, RefPlace, STARTING, Size, Stmt, StreamMap, StreamRead, Streaming,
    TYPES, Turn, Turning, Ty, Via, Writing,
};
use super::ast::{Calling, Field, FieldCall, FieldCalls, Waking, Yielding};

/// Whether a crash runs a `lend x except:` cleanup holding a line like `kept = capped(code)?`.
/// It does since #1333; turned off, only a block that fails is given `guarded_noted`'s cleanup.
const CRASH_RUNS_A_CLEANUP_WITH_QUESTION_MARK: bool = true;

/// Whether `x = f() else return`, `else fail`, `else continue` and `else break` run the cleanups
/// of the `with` blocks they leave. They do since #1341; turned off, `Left` leaves its blocks
/// every other way.
const ELSE_LEAVES_RUN_CLEANUPS: bool = true;

/// Whether random programs read a generator over `parallel(xs)`, which runs as the runtime's
/// ordered stream since #1479. Turned off, no input reads the byte that picks it, so the shape
/// can be taken out without changing what any other input writes.
const PARALLEL_STREAMS: bool = true;

/// Whether random programs make copies of collections and their parts, change each through every
/// way of writing in place, and print them all: the shape that shows an in-place write reaching a
/// second holder. On since the LLVM lane's in-place writes and their fixes (#1543, #1558).
const ALIASING: bool = true;

/// Whether random programs read module constants of lists, dicts and structs on the main thread,
/// then use and change copies of them, through a struct's field defaults too, in launched tasks
/// and `parallel`'s bodies. On since the runtime's count fix for never-freed blocks (#1531).
const CONSTANTS_IN_TASKS: bool = true;

/// Whether random programs make `box.field += f(...)` on a struct's `str`, `int` and list fields,
/// with calls that print, fail, or read the field through a copy, a list or a closure, and on a
/// `shared` struct's: the field's read comes after the call, so this shows if the order or a
/// failure leaves the field wrong. On since a failing piece keeps what was there (#1558).
const FIELD_CALLS: bool = true;

/// Whether random programs have generators yield structs, tuples and enums holding texts past
/// 23 bytes, lists and dicts, keep a struct in a local across yields, and send what they yield
/// through a task's inbox loop and back. On since a generator's slot owns what it holds (#1560).
const YIELDS: bool = true;

/// Whether random programs launch a task that crashes or fails while its launcher waits on a
/// channel for its answer, which has to end the wait rather than hang. On since #1563.
const CRASH_WAKES: bool = true;

/// Whether a call handling several of `Crashed`, `Closed`, `Timeout` and `Refused` prints each
/// one's own word, `crashed`, `closed`, `timeout` or `refused`, rather than one word for them all,
/// so a task's failure that comes out as the wrong one shows. Off until a launched task that fails
/// while its block still sends ends the block with its own failure rather than `Closed`, which
/// the checker lane is fixing; with it on, `guard_all` finds that on main.
pub(crate) const DISTINCT_STOPS: bool = false;

/// Whether a failure passed up by a `?` nested inside another call's argument, as in
/// `capped(capped(i, limit=-1)?, limit=1000)?`, runs the cleanups of the `with` block it leaves.
/// It does since #1399; turned off, `Left` nests `?` only with the outer call failing.
const INNER_QUESTION_RUNS_CLEANUPS: bool = true;

/// Whether leaving a `with` block from a `case` arm under `except why:`, by `return`, `break` or
/// `continue`, runs the code after its source's `lend`. It does since #1418; turned off, `Left`
/// leaves only from an `except why:` block of plain statements.
const EXCEPT_ARMS_LEAVE_RUN_CLEANUPS: bool = true;

/// How many statements a program has before it ends, where the input's bytes run out sooner.
const MOST_FROM_STREAM: usize = 60;

/// A stream's start from the input's bytes, never zero, which xorshift can't leave.
fn seeded(bytes: &[u8]) -> u64 {
    let hash = fennec_blake3::hash(bytes);
    let first: [u8; 8] = hash.as_bytes().get(..8).and_then(|head| head.try_into().ok()).unwrap_or([1; 8]);
    u64::from_le_bytes(first) | 1
}

/// How deep expressions nest.
const MOST_DEPTH: usize = 3;
/// How deep blocks nest.
const MOST_BLOCKS: usize = 3;
/// What prints a value: a buffer, which can't be printed whole, by its items, a deep tree by how
/// deep it is, and where the program never prints a tree, as the native-blocks work needs some
/// programs to, a tree by its digest.
fn printable(value: Expr, ty: Ty, digest_only: bool) -> Expr {
    match ty {
        Ty::Buf => Expr::BufItems(Box::new(value)),
        Ty::Deep => Expr::DepthOf(Box::new(value)),
        // Never printed whole, in every program, so it qualifies for native blocks.
        Ty::Named => Expr::NamedDigest(Box::new(value)),
        Ty::Chain => Expr::NamedDepth(Box::new(value)),
        Ty::Node if digest_only => Expr::Digest(Box::new(value)),
        Ty::Nodes if digest_only => Expr::DigestAll(Box::new(value)),
        _ => value,
    }
}

/// Whether a value is a tree or a list of them, whose names an expression of either holds at
/// most one of, so trees grow by adding, never by doubling, however a loop goes.
const fn is_tree(ty: Ty) -> bool {
    matches!(ty, Ty::Node | Ty::Nodes | Ty::Named)
}

/// `n` made to fit `size`, for a literal, which has to.
fn literal_in(size: Size, n: i64) -> i64 {
    match size {
        Size::U8 => n.rem_euclid(256),
        Size::I32 => i64::from(i32::try_from(n.rem_euclid(1 << 31)).unwrap_or(0)) * n.signum(),
        Size::U64 => n.saturating_abs(),
    }
}

/// What a worked-out int is kept below, with `%`, to fit `size` where nothing's meant to fail.
const fn inside(size: Size) -> i64 {
    match size {
        Size::U8 => 256,
        Size::I32 => 1 << 31,
        Size::U64 => 1 << 62,
    }
}

/// The keys a `Bag`'s dict holds.
const KEYS: [&str; 3] = ["a", "b", "c"];
/// The most statements a program has, past which a block gets none: `if` and `else` in each
/// other could otherwise make one too big to build in a minute.
const MOST_IN_ALL: usize = 250;
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
    /// How many statements have been written.
    written: usize,
    /// What chooses once the input's bytes run out: a stream seeded by them, so the input still
    /// decides everything, and a short one doesn't make a program of nothing but its first choice.
    stream: u64,
    /// Whether the program never prints a tree whole, only its digest.
    digest_only: bool,
    /// Whether the program launches no task and runs nothing `parallel`.
    threadless: bool,
    /// How many more names of trees the expression being written can hold.
    tree_names: usize,
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
    let digest_only = bytes.first().is_some_and(|first| first & 0x10 != 0);
    // One program in four has no other threads, which an enum holding strings needs to be made
    // of native blocks.
    let threadless = bytes.first().is_some_and(|first| first & 0x60 == 0);
    let bytes = bytes.get(1..).unwrap_or_default();
    let mut writer = Writer {
        bytes: bytes.iter(),
        names: Vec::new(),
        made: 0,
        risky,
        loops: 0,
        written: 0,
        stream: seeded(bytes),
        digest_only,
        threadless,
        tree_names: 1,
    };
    let mut body = Vec::new();
    while (writer.bytes.len() > 0 || writer.written < MOST_FROM_STREAM)
        && body.len() < 64
        && writer.written <= MOST_IN_ALL
    {
        writer.statement(&mut body, 0);
    }
    writer.print_from(&mut body, 0);
    Program { body, threadless }
}

impl Writer<'_> {
    /// The next choice: the input's next byte, then the stream's.
    fn byte(&mut self) -> u8 {
        if let Some(byte) = self.bytes.next() {
            return *byte;
        }
        // xorshift64, which only has to be the same every time.
        self.stream ^= self.stream << 13;
        self.stream ^= self.stream >> 7;
        self.stream ^= self.stream << 17;
        self.stream.to_le_bytes().first().copied().unwrap_or(0)
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
        match self.choice(22) {
            14 => {
                let size = self.size();
                Expr::IntOf(Box::new(self.of(Ty::Sized(size), next)))
            }
            15 => {
                let limit = if self.risky {
                    Expr::Arith(Arith::Rem, Box::new(self.int(next)), Box::new(Expr::Int(9)))
                } else {
                    Expr::Int(i64::from(self.byte() % 8))
                };
                Expr::SumCounted(Box::new(limit))
            }
            16 => self.name_of(Ty::Tally, false).map_or_else(|| self.small_int(), Expr::Count),
            19 => {
                let key = KEYS.get(self.choice(KEYS.len())).copied().unwrap_or("a");
                Expr::CountOf(Box::new(self.of(Ty::Counts, next)), key)
            }
            21 if !self.threadless => {
                let list = self.of(Ty::Ints, next);
                let by = if self.risky { self.int(next) } else { Expr::Int(i64::from(self.byte() % 4)) };
                // Half pass each item's holder a list read from itself, on the threads.
                let sum = if self.choice(2) == 0 {
                    Expr::Regrown(Box::new(list))
                } else {
                    Expr::ParallelSum(Box::new(list), Box::new(by))
                };
                self.bounded(sum)
            }
            20 if self.risky => Expr::BufAt(Box::new(self.of(Ty::Buf, next)), Box::new(self.int(next))),
            20 => Expr::BufLen(Box::new(self.of(Ty::Buf, next))),
            17 if self.risky => Expr::Truncated(Box::new(self.float(next))),
            17 => Expr::Tag(Box::new(self.of(Ty::Holder, next))),
            18 => {
                let called = self.name_of(Ty::Count, false).map(Expr::Called);
                let called = called.unwrap_or_else(|| self.small_int());
                self.bounded(called)
            }
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
            && (!is_tree(ty) || self.tree_names > 0)
            && let Some(name) = self.name_of(ty, false).filter(|_| depth >= MOST_DEPTH || self.choice(2) == 0)
        {
            if is_tree(ty) {
                self.tree_names -= 1;
            }
            // Stored in a loop, it's copied each round, which is a hint, not a mistake.
            return Expr::Name(name);
        }
        let next = depth + 1;
        match ty {
            Ty::Int => self.int(depth),
            Ty::Bool if depth >= MOST_DEPTH => Expr::Bool(self.choice(2) == 0),
            Ty::Bool => match self.choice(6) {
                0 => Expr::Bool(self.choice(2) == 0),
                5 => {
                    let key = KEYS.get(self.choice(KEYS.len())).copied().unwrap_or("a");
                    Expr::Has(Box::new(self.of(Ty::Counts, next)), key)
                }
                4 => {
                    const COMPARES: [Compare; 5] =
                        [Compare::Less, Compare::LessEq, Compare::Equal, Compare::NotEqual, Compare::Greater];
                    let compare = COMPARES.get(self.choice(COMPARES.len())).copied().unwrap_or(Compare::Less);
                    let (left, right) = (self.float(next), self.float(next));
                    // A float compared with itself is refused, since NaN equals NaN.
                    let right = match (&left, right) {
                        (Expr::Name(a), Expr::Name(b)) if *a == b => Expr::Float(0.5),
                        (_, right) => right,
                    };
                    Expr::FloatCompare(compare, Box::new(left), Box::new(right))
                }
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
            Ty::Ints => match self.choice(7) {
                0 => {
                    let count = self.choice(4);
                    Expr::Ints((0..count).map(|_| self.int(next)).collect())
                }
                1 => {
                    let list = self.of(Ty::Ints, next);
                    let by = if self.risky { self.int(next) } else { Expr::Int(i64::from(self.byte() % 4)) };
                    let plus = self.int(MOST_DEPTH);
                    let plus = self.bounded(plus);
                    let parallel = self.choice(3) == 0 && !self.threadless;
                    Expr::Mapped(Box::new(list), Box::new(by), Box::new(plus), parallel)
                }
                2 => Expr::Ints(Vec::new()),
                3 => Expr::HeldItems(Box::new(self.of(Ty::Holder, next))),
                6 => Expr::BufItems(Box::new(self.of(Ty::Buf, next))),
                4 => {
                    let key = KEYS.get(self.choice(KEYS.len())).copied().unwrap_or("a");
                    Expr::ListAt(Box::new(self.of(Ty::Lists, next)), key)
                }
                _ => Expr::Ints(vec![Expr::Int(3), Expr::Int(1), Expr::Int(2)]),
            },
            Ty::Point => Expr::Point(Box::new(self.int(next)), Box::new(self.int(next))),
            Ty::Node if depth >= MOST_DEPTH => {
                if self.choice(2) == 0 {
                    Expr::Leaf(Box::new(self.of(Ty::Str, next)))
                } else {
                    Expr::Number(Box::new(self.int(next)))
                }
            }
            Ty::Node => match self.choice(6) {
                0 => Expr::Leaf(Box::new(self.of(Ty::Str, next))),
                // In a risky program the number can overflow as it's worked out, failing partway
                // through building the tree around it.
                1 => Expr::Number(Box::new(self.int(next))),
                2 | 3 => Expr::Branch(Box::new(self.of(Ty::Str, next)), Box::new(self.of(Ty::Nodes, next))),
                4 => {
                    let count = self.choice(3);
                    let tags = (0..count).map(|_| self.of(Ty::Str, next)).collect();
                    Expr::Tagged(Box::new(Expr::Texts(tags)), Box::new(self.of(Ty::Nodes, next)))
                }
                _ => Expr::NodeAt(Box::new(self.of(Ty::Nodes, next)), Box::new(self.int(next))),
            },
            Ty::Nodes if depth >= MOST_DEPTH => Expr::NodeList(Vec::new()),
            Ty::Nodes => {
                let count = self.choice(4);
                Expr::NodeList((0..count).map(|_| self.of(Ty::Node, next)).collect())
            }
            Ty::Deep => Expr::Deep(0, Box::new(Expr::Str("d".to_owned()))),
            Ty::Named if depth >= MOST_DEPTH => Expr::NamedLeaf(Box::new(self.of(Ty::Str, next))),
            Ty::Named => match self.choice(5) {
                0 => Expr::NamedLeaf(Box::new(self.of(Ty::Str, next))),
                1 => Expr::Chain(i64::from(self.byte() % 9), Box::new(self.of(Ty::Str, next))),
                _ => {
                    let (left, right) = (self.of(Ty::Named, next), self.of(Ty::Named, next));
                    let count = self.choice(3);
                    let tags = (0..count).map(|_| self.of(Ty::Str, next)).collect();
                    let label = self.of(Ty::Str, next);
                    Expr::NamedPair(
                        Box::new(left),
                        Box::new(right),
                        Box::new(label),
                        Box::new(Expr::Texts(tags)),
                    )
                }
            },
            Ty::Chain => Expr::Chain(65, Box::new(Expr::Str("c".to_owned()))),
            Ty::Sized(size) if depth >= MOST_DEPTH => self.sized_literal(size),
            Ty::Sized(size) => match self.choice(4) {
                0 => {
                    let value = self.int(next);
                    // A literal is checked to fit when the program's compiled, so only what's
                    // worked out can be out of range, and only where the program's risky.
                    let value = match value {
                        Expr::Int(n) => Expr::Int(literal_in(size, n)),
                        value if self.risky => value,
                        value => Expr::Arith(Arith::Rem, Box::new(value), Box::new(Expr::Int(inside(size)))),
                    };
                    Expr::Convert(size, Box::new(value))
                }
                1 => Expr::SizedWrap(
                    size,
                    Box::new(self.of(Ty::Sized(size), next)),
                    Box::new(self.of(Ty::Sized(size), next)),
                ),
                2 if self.risky => {
                    let operator = [Arith::Add, Arith::Sub, Arith::Mul]
                        .get(self.choice(3))
                        .copied()
                        .unwrap_or(Arith::Add);
                    let (left, right) = (self.of(Ty::Sized(size), next), self.of(Ty::Sized(size), next));
                    Expr::SizedArith(size, operator, Box::new(left), Box::new(right))
                }
                _ => self.sized_literal(size),
            },
            // These are made by statements of their own, never in an expression.
            Ty::Holder => {
                let items = self.of(Ty::Ints, next);
                Expr::Holder(Box::new(items), Box::new(self.int(next)))
            }
            Ty::Lists => {
                let count = self.choice(3);
                let pairs = KEYS.iter().take(count).map(|key| (*key, self.of(Ty::Ints, next))).collect();
                Expr::Lists(pairs)
            }
            Ty::Float => self.float(depth),
            Ty::Buf => {
                let capacity = if self.choice(2) == 0 { None } else { Some(i64::from(self.byte() % 9)) };
                let count = self.choice(4);
                Expr::Buffer(capacity, (0..count).map(|_| self.int(next)).collect())
            }
            Ty::Counts => {
                let count = self.choice(4);
                let pairs = KEYS.iter().take(count).map(|key| (*key, self.int(next))).collect();
                Expr::Counts(pairs)
            }
            Ty::Shared | Ty::Tally | Ty::Bag | Ty::Count | Ty::Outer => Expr::Int(0),
            Ty::Shape => match self.choice(3) {
                0 => Expr::Shape(Case::Circle, Box::new(self.int(next))),
                1 => Expr::Shape(Case::Square, Box::new(self.int(next))),
                _ => Expr::Shape(Case::Empty, Box::new(Expr::Int(0))),
            },
        }
    }

    fn size(&mut self) -> Size {
        [Size::U8, Size::I32, Size::U64].get(self.choice(3)).copied().unwrap_or(Size::U8)
    }

    /// A sized integer written out as a conversion of a literal that fits.
    fn sized_literal(&mut self, size: Size) -> Expr {
        let n = i64::from(self.byte()) * if self.choice(2) == 0 { 1 } else { -1_000_003 };
        Expr::Convert(size, Box::new(Expr::Int(literal_in(size, n))))
    }

    /// A float: written out, worked out, or from an int.
    fn float(&mut self, depth: usize) -> Expr {
        const FLOATS: [f64; 9] = [0.5, 1.5, -2.25, 1e10, 0.1, 3.0, -0.0, 1e300, 2.5e-5];
        const DIVISORS: [f64; 4] = [0.5, 3.0, -2.25, 1e-300];
        let literal =
            |at: usize, from: &[f64]| Expr::Float(from.get(at % from.len()).copied().unwrap_or(1.0));
        if depth >= MOST_DEPTH {
            return self
                .name_of(Ty::Float, false)
                .map_or_else(|| literal(self.choice(9), &FLOATS), Expr::Name);
        }
        let next = depth + 1;
        match self.choice(6) {
            0 | 1 => literal(self.choice(9), &FLOATS),
            2 => {
                let operator =
                    [Arith::Add, Arith::Sub, Arith::Mul].get(self.choice(3)).copied().unwrap_or(Arith::Add);
                Expr::FloatArith(operator, Box::new(self.float(next)), Box::new(self.float(next)))
            }
            3 => {
                // A divisor written out is never zero, which could be refused as the program's
                // compiled; one worked out may be, in a risky program, and fails as it runs.
                let divisor = if self.risky { self.float(next) } else { literal(self.choice(4), &DIVISORS) };
                Expr::FloatArith(Arith::Div, Box::new(self.float(next)), Box::new(divisor))
            }
            4 => Expr::FloatOf(Box::new(self.int(next))),
            _ => self.name_of(Ty::Float, false).map_or_else(|| literal(self.choice(9), &FLOATS), Expr::Name),
        }
    }

    fn ty(&mut self) -> Ty {
        TYPES.get(self.choice(TYPES.len())).copied().unwrap_or(Ty::Int)
    }

    /// A block's statements, its names printed at its end and then forgotten.
    fn block(&mut self, depth: usize) -> Vec<Stmt> {
        let scope = self.names.len();
        let mut body = Vec::new();
        let count = if self.written > MOST_IN_ALL { 0 } else { 1 + self.byte() % MOST_STATEMENTS };
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
        for name in self.names.iter().skip(scope) {
            match name.ty {
                Ty::Shared | Ty::Bag => body.push(Stmt::PrintShared(name.name.clone())),
                Ty::Outer => body.push(Stmt::PrintOuter(name.name.clone())),
                _ => {}
            }
        }
        let values: Vec<(Expr, Ty)> = self
            .names
            .iter()
            .skip(scope)
            .filter(|name| !matches!(name.ty, Ty::Shared | Ty::Tally | Ty::Bag | Ty::Count | Ty::Outer))
            .map(|name| (printable(Expr::Name(name.name.clone()), name.ty, self.digest_only), name.ty))
            .collect();
        for chunk in values.chunks(4) {
            body.push(Stmt::Print(chunk.to_vec()));
        }
    }

    fn statement(&mut self, body: &mut Vec<Stmt>, depth: usize) {
        self.written += 1;
        self.tree_names = 1;
        let which = self.choice(36);
        match which {
            30 | 31 => self.trees(body),
            32 | 33 => self.local_refs(body),
            34 | 35 => self.named(body),
            3..=5 | 7 | 19..=21 if depth < MOST_BLOCKS => self.nested(body, depth, which),
            23..=25 => self.copies(body, which),
            26 if self.choice(3) == 0 => self.leaves(body),
            26 if self.choice(4) == 0 => self.turns(body),
            26 if PARALLEL_STREAMS && !self.threadless && self.choice(4) == 0 => self.streams(body),
            26 if ALIASING && self.choice(4) == 0 => self.aliasing(body),
            26 => self.change_counts(body),
            // The guarded shapes are tried first, as they were before the shapes after them, so
            // each of their cases still comes up as often.
            27 if self.choice(3) == 0 => self.guards(body),
            27 if CONSTANTS_IN_TASKS && !self.threadless && self.choice(4) == 0 => self.constants(body),
            27 if FIELD_CALLS && self.choice(4) == 0 => self.field_calls(body),
            27 if YIELDS && !self.threadless && self.choice(4) == 0 => self.yields(body),
            27 if CRASH_WAKES && !self.threadless && self.choice(4) == 0 => self.wakes(body),
            27 => self.change_buffer(body),
            29 => {
                let table = self.fresh();
                let count = 1 + self.choice(3);
                let ids: Vec<String> = (0..count).map(|_| self.fresh()).collect();
                let xs = (0..count).map(|_| self.int(1)).collect();
                let removed = if count > 1 && self.choice(3) == 0 { Some(self.choice(count)) } else { None };
                // The point read by its `Id` is never the one taken out, which would fail.
                let start = self.choice(count);
                let read = (0..count)
                    .map(|step| (start + step) % count)
                    .find(|at| Some(*at) != removed)
                    .unwrap_or(0);
                let by = self.int(1);
                let by = self.bounded(by);
                body.push(Stmt::TableRound { table, ids, xs, removed, read, by });
            }
            28 if self.choice(2) == 0 => self.arrows_or_floats(body),
            28 => {
                // A generator lending from a source, let go partway or run out.
                let limit = Expr::Int(i64::from(self.byte() % 6));
                let stop = Expr::Int(i64::from(self.byte() % 10));
                body.push(Stmt::Summed(limit, stop));
            }
            22 => {
                let Some(list) = self.name_of(Ty::Ints, true) else { return self.print(body) };
                let by = if self.risky { self.small_int() } else { Expr::Int(i64::from(self.byte() % 5)) };
                body.push(Stmt::GrowAll(list.clone(), by));
                self.read_back(body, &list);
            }
            16..=18 => self.sharing(body, which),
            0..=2 => {
                let ty = self.ty();
                // In a loop, holding a name's value while the name changes copies it each round,
                // which is a hint, not a mistake.
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
                // Half read the list they're changing, at a fixed place or where the item says.
                let read = [None, Some(0), Some(-1), Some(2)].get(self.choice(4)).copied().flatten();
                body.push(match self.choice(3) {
                    0 => Stmt::EachRef(item, list.clone(), by),
                    1 => Stmt::EachRefRead(item, list.clone(), read),
                    _ => Stmt::EachRefThenRead(item, list.clone(), read.unwrap_or(0)),
                });
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
                let add = ty == Ty::Float && self.choice(2) == 0;
                body.push(Stmt::Assign { place: Place::Name(name.clone()), value, add });
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
            21 => {
                // The list may change in the body: that's the point.
                let Some(list) = self.name_of(Ty::Ints, true) else { return self.print(body) };
                let name = self.fresh();
                let inner = self.round(&name, Ty::Int, depth);
                body.push(Stmt::Indexed(name, list, inner));
            }
            _ => {
                let name = self.fresh();
                let start = self.int(1);
                self.names.push(InScope { name: name.clone(), ty: Ty::Tally, changeable: false, held: 0 });
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
        // A loop over a name's list, or over a holder's, holds that name, which can't change under it.
        let looped = match &list {
            Expr::Name(looped) => Some(looped.clone()),
            Expr::HeldItems(holder) => match holder.as_ref() {
                Expr::Name(looped) => Some(looped.clone()),
                _ => None,
            },
            _ => None,
        };
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
        // Half of these are about a `shared Bag`, whose list and dict change under its lock.
        if self.choice(2) == 0 {
            return self.bag(body, which);
        }
        if which == 16 {
            let name = self.fresh();
            let (x, y) = (self.int(1), self.int(1));
            body.push(Stmt::Share(name.clone(), x, y));
            self.names.push(InScope { name, ty: Ty::Shared, changeable: false, held: 0 });
            return;
        }
        let Some(shared) = self.name_of(Ty::Shared, false) else { return self.print(body) };
        if which == 18 && !self.threadless {
            let items = self.of(Ty::Ints, 1);
            body.push(Stmt::Tally(shared, items));
            return;
        }
        let by = self.int(1);
        let by = self.bounded(by);
        let refuse = self.of(Ty::Bool, 1);
        body.push(Stmt::Change(shared, by, refuse));
    }

    /// A `shared Bag`, or changes to one under its lock, from `main` or from a task.
    fn bag(&mut self, body: &mut Vec<Stmt>, which: usize) {
        if which == 16 || self.name_of(Ty::Bag, false).is_none() {
            let name = self.fresh();
            let items = self.of(Ty::Ints, 1);
            let count = self.choice(3);
            let counts = KEYS.iter().take(count).map(|key| (*key, i64::from(self.byte() % 9))).collect();
            body.push(Stmt::ShareBag(name.clone(), items, counts));
            self.names.push(InScope { name, ty: Ty::Bag, changeable: false, held: 0 });
            return;
        }
        let Some(name) = self.name_of(Ty::Bag, false) else { return self.print(body) };
        // Now and then a `shared Outer` holding it, or a change to one under its own lock.
        match self.choice(5) {
            0 => {
                let outer = self.fresh();
                body.push(Stmt::ShareOuter(outer.clone(), name));
                self.names.push(InScope { name: outer, ty: Ty::Outer, changeable: false, held: 0 });
                return;
            }
            1 => {
                let Some(outer) = self.name_of(Ty::Outer, false) else { return self.print(body) };
                let by = self.int(1);
                let by = self.bounded(by);
                let refuse = self.of(Ty::Bool, 1);
                body.push(Stmt::OuterBump(outer, by, refuse));
                return;
            }
            _ => {}
        }
        self.made += 1;
        let id = self.made;
        if which == 17 || self.threadless {
            // From `main`, a crash would end the program, where the undoing can't be seen, so
            // only a risky program's changes can crash there.
            let ops = self.ops(!self.risky, false);
            let refuse = self.of(Ty::Bool, 1);
            body.push(Stmt::Locked { id, name, ops, refuse });
        } else {
            let ops = self.ops(false, true);
            let items = self.of(Ty::Ints, 1);
            body.push(Stmt::Sent { id, name, ops, items });
        }
    }

    /// One to four changes under a bag's lock: only ones that can't crash where `safe` says so,
    /// and using the number a task was sent where `sent` says so.
    fn ops(&mut self, safe: bool, sent: bool) -> Vec<Op> {
        const PLACES: [i64; 5] = [0, 1, -1, 2, 5];
        let count = 1 + self.choice(4);
        let mut ops = Vec::with_capacity(count);
        for _ in 0..count {
            let value = if sent && self.choice(2) == 0 {
                Amount::Sent(i64::from(self.byte() % 4))
            } else {
                Amount::Literal(i64::from(self.byte() % 20) - 5)
            };
            let key = KEYS.get(self.choice(KEYS.len())).copied().unwrap_or("a");
            let at = PLACES.get(self.choice(PLACES.len())).copied().unwrap_or(0);
            let op = match self.choice(12) {
                9 => Op::CountsCopied(key, value),
                10 => Op::SelfAssigned,
                11 if !safe => Op::PopCount(key),
                8 if !safe => Op::RefItem(at, value),
                6 => Op::AddTwice(value),
                7 => Op::GrowItems(value),
                0 | 1 | 8 => Op::Append(value),
                2 => Op::Pop,
                3 if safe => Op::SetCount(key, value, false),
                3 => Op::SetItem(at, value, self.choice(2) == 0),
                4 => Op::SetCount(key, value, !safe && self.choice(2) == 0),
                _ => Op::Remove(key),
            };
            ops.push(op);
        }
        ops
    }

    /// A list lent to a call through `ref` beside a value read from it, the whole list or a
    /// list's item, or a value assigned to itself, a local or a struct's fields.
    /// A `with` whose source's `lend x except:` cleanup runs when the block fails or crashes, in
    /// `main` or in a launched task.
    fn guards(&mut self, body: &mut Vec<Stmt>) {
        if !self.threadless && self.choice(3) == 0 {
            let count = 1 + self.choice(5);
            let items = (0..count).map(|_| i64::from(self.byte() % 10)).collect();
            body.push(Stmt::GuardAll(items));
            return;
        }
        let code = i64::from(self.byte() % 32) - 4;
        let refuse = self.choice(2) == 0;
        // A crash ends the program, so it's rare.
        let crash = self.choice(8) == 0;
        let noted = self.choice(2) == 0 && (CRASH_RUNS_A_CLEANUP_WITH_QUESTION_MARK || !crash);
        body.push(Stmt::Guarded { code, refuse, crash, noted });
    }

    /// `with` blocks over sources the program writes, nested or together, all left at once by
    /// one of the ways out of a block.
    fn leaves(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let depth = 1 + self.choice(3);
        let together = depth > 1 && self.choice(3) == 0;
        let rounds = 1 + i64::from(self.byte() % 4);
        let at = i64::from(self.byte()) % (rounds + 1);
        let mut ways = vec![
            Leave::Question,
            Leave::QuestionInside,
            Leave::QuestionNestedOuter,
            Leave::NestedReturn,
            Leave::Return,
            Leave::Break,
            Leave::Continue,
        ];
        if INNER_QUESTION_RUNS_CLEANUPS {
            ways.push(Leave::QuestionNestedInner);
        }
        if ELSE_LEAVES_RUN_CLEANUPS {
            ways.extend([Leave::ElseReturn, Leave::ElseFail, Leave::ElseContinue, Leave::ElseBreak]);
        }
        ways.extend([Leave::ExceptReturn, Leave::ExceptContinue, Leave::ExceptValue]);
        if EXCEPT_ARMS_LEAVE_RUN_CLEANUPS {
            ways.push(Leave::ExceptArms);
        }
        // A crash ends the program, so it's rare.
        let how = if self.choice(10) == 0 {
            Leave::Crash
        } else {
            ways.get(self.choice(ways.len())).copied().unwrap_or(Leave::Return)
        };
        body.push(Stmt::Left { id, depth, together, rounds, at, how });
    }

    /// A generator over `parallel(xs)`, an ordered stream, read by a `for` loop that may break, by
    /// `sum`, or by a list comprehension. Its items may fail by dividing by zero or indexing past
    /// a list, which stops the program at the first one read.
    fn streams(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let count = 3 + usize::from(self.byte() % 6);
        let items: Vec<i64> = (0..count).map(|_| i64::from(self.byte() % 7)).collect();
        let map = match self.choice(3) {
            0 => StreamMap::Scaled(i64::from(self.byte() % 7) - 3, i64::from(self.byte() % 11) - 5),
            1 => StreamMap::Divided(i64::from(self.byte() % 7)),
            _ => StreamMap::Indexed,
        };
        let read = match self.choice(4) {
            0 => StreamRead::Sum,
            1 => StreamRead::Listed,
            2 => StreamRead::Loop(Some(1 + i64::from(self.byte()) % i64::try_from(count).unwrap_or(1))),
            _ => StreamRead::Loop(None),
        };
        body.push(Stmt::Streamed(Streaming { id, items, map, read }));
    }

    /// A task answering numbers over a channel that crashes or fails on one, while its launcher
    /// waits for the answer, or answers them all.
    fn wakes(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let count = 1 + i64::from(self.byte() % 5);
        let at = i64::from(self.byte() % 7);
        let failing = self.choice(2) == 0;
        let rounds = if self.choice(4) == 0 { 70 } else { 1 };
        body.push(Stmt::Woken(Waking { id, count, at, failing, rounds }));
    }

    /// Generators yielding structs, tuples and enums holding texts from 20 to 66 bytes, and an
    /// inbox loop sending what they yielded back.
    fn yields(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let count = 2 + i64::from(self.byte() % 5);
        let mut width = || 20 + i64::from(self.byte() % 47);
        let widths = [width(), width(), width()];
        body.push(Stmt::Yielded(Yielding { id, count, widths }));
    }

    /// `+=` on a struct's fields, each with a call on its right that prints, fails, or reads the
    /// field through another path, or on a `shared` struct's under `lock`.
    fn field_calls(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let base = i64::from(self.byte() % 20);
        let mut steps = Vec::new();
        for _ in 0..4 + self.choice(8) {
            let field = match self.choice(3) {
                0 => Field::Text,
                1 => Field::Count,
                _ => Field::Items,
            };
            let call = match self.choice(6) {
                0 => Calling::Printed,
                1 => Calling::Failed,
                2 => Calling::Copied,
                3 => Calling::Listed,
                4 => Calling::Closure,
                _ => Calling::Shared,
            };
            let plus = self.choice(2) == 0;
            let n = i64::from(self.byte() % 9);
            steps.push(FieldCall { field, call, plus, n });
        }
        body.push(Stmt::FieldCalls(FieldCalls { id, base, steps }));
    }

    /// Module constants read on the main thread `reads` times, then used and changed in copies
    /// on the main thread, in a launched task and in `parallel`'s bodies, in an order of its own.
    fn constants(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let base = i64::from(self.byte() % 20);
        let reads = i64::from(self.byte() % 4);
        let main_first = self.choice(2) == 0;
        let launch_first = self.choice(2) == 0;
        body.push(Stmt::Constants(Constants { id, base, reads, main_first, launch_first }));
    }

    /// Copies of collections and of their parts, each name then changed through the ways of
    /// writing in place its kind takes, walked, handed to tasks, and all of them printed. Every
    /// write reaches only the name it's written to, so a write that reaches a copy too shows in
    /// what's printed.
    fn aliasing(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let base = i64::from(self.byte() % 20);
        let mut held: Vec<Held> = STARTING.iter().map(|(_, held)| *held).collect();
        let mut steps = Vec::new();
        for _ in 0..6 + self.choice(13) {
            let from = self.choice(held.len());
            let kind = held.get(from).copied().unwrap_or(Held::Numbers);
            let roll = self.choice(6);
            if roll == 0 {
                steps.push(Alias::Copied { from });
                held.push(kind);
                continue;
            }
            // A copy of a table keeps its tag, so the starting `Id`s find items in copies too.
            if roll == 1
                && let Some(part) = kind.part()
            {
                steps.push(Alias::Part { from, at: self.choice(2) });
                held.push(part);
                continue;
            }
            if roll == 2 && kind.keyed() && self.choice(2) == 0 {
                let (at, n) = (self.choice(2), i64::from(self.byte() % 19) - 9);
                steps.push(Alias::Drained { from, at, n });
                continue;
            }
            if roll == 2 && kind.walked() {
                steps.push(Alias::Walked { from });
                continue;
            }
            if roll == 2 && kind == Held::Numbers {
                steps.push(Alias::Built { from, n: i64::from(self.byte() % 19) - 9 });
                held.push(Held::Holder);
                continue;
            }
            let ways: &[Writing] = match kind {
                Held::Numbers => &[Writing::Append, Writing::Set, Writing::ForRef, Writing::Ref],
                Held::Holders => {
                    &[Writing::ForRef, Writing::Field, Writing::Append, Writing::PutBack, Writing::Method]
                }
                Held::Keepers => &[
                    Writing::Field,
                    Writing::Append,
                    Writing::Set,
                    Writing::PutBack,
                    Writing::Method,
                    Writing::Removed,
                ],
                Held::Holder => &[Writing::Field, Writing::Append, Writing::Method],
                Held::Lists => &[Writing::Set, Writing::PutBack, Writing::Append, Writing::Removed],
                Held::Spots => &[Writing::Field, Writing::PutBack, Writing::Method, Writing::Removed],
                Held::Counts => &[Writing::Field, Writing::Set, Writing::Removed],
                Held::Rooms => {
                    &[Writing::ForRef, Writing::Set, Writing::Field, Writing::PutBack, Writing::Method]
                }
                Held::Table | Held::Entries => {
                    &[Writing::ForRef, Writing::Set, Writing::PutBack, Writing::Method]
                }
                Held::Ledger => {
                    &[Writing::Field, Writing::ForRef, Writing::Set, Writing::PutBack, Writing::Method]
                }
                Held::Point => &[Writing::Field, Writing::Method],
                Held::Text => &[
                    Writing::Joined,
                    Writing::Added,
                    Writing::Formatted,
                    Writing::Chained,
                    Writing::Summed,
                    Writing::Long,
                ],
            };
            // A table read by a loop that borrows each item from its slot.
            if matches!(kind, Held::Table | Held::Entries | Held::Ledger | Held::Rooms) && self.choice(3) == 0
            {
                // A ledger's table can't be replaced while it's looped over, since that changes
                // the ledger.
                let ways: &[Reading] = if kind == Held::Rooms {
                    &[Reading::Summed, Reading::Broken]
                } else if kind == Held::Ledger {
                    &[Reading::Summed, Reading::Kept, Reading::Broken, Reading::Returned, Reading::Failed]
                } else {
                    &[
                        Reading::Summed,
                        Reading::Kept,
                        Reading::Replaced,
                        Reading::Broken,
                        Reading::Returned,
                        Reading::Failed,
                    ]
                };
                let how = ways.get(self.choice(ways.len())).copied().unwrap_or(Reading::Summed);
                let n = i64::from(self.byte() % 30) - 5;
                steps.push(Alias::Read { from, how, n });
                if how == Reading::Kept {
                    held.push(Held::Point);
                }
                continue;
            }
            // Any collection can be handed to tasks, each changing a copy of its own.
            if !matches!(kind, Held::Point | Held::Text) && !self.threadless && self.choice(5) == 0 {
                steps.push(Alias::Tasked { from });
                continue;
            }
            let how = ways.get(self.choice(ways.len())).copied().unwrap_or(Writing::ForRef);
            let at = self.choice(2);
            let n = i64::from(self.byte() % 19) - 9;
            steps.push(Alias::Written { to: from, how, at, n });
        }
        body.push(Stmt::Aliased(Aliasing { id, base, steps }));
    }

    /// A loop whose int is at least 0 on its first turn and changes sign later, used as an index,
    /// in a multiply and in adds at each end of `int`. Most stay from -4 to 3, where all of
    /// those hold; some step past it, which stops the program.
    fn turns(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let how = match self.choice(3) {
            0 => Turn::StepDown,
            1 => Turn::Flip,
            _ => Turn::Reset,
        };
        let rounds = 2 + i64::from(self.byte() % 4);
        let start = i64::from(self.byte() % 4);
        let step = 1 + i64::from(self.byte() % 2);
        let reset_at = 1 + i64::from(self.byte()) % (rounds - 1);
        let reset_to = -1 - i64::from(self.byte() % 4);
        body.push(Stmt::Turned(Turning { id, how, rounds, start, step, reset_at, reset_to }));
    }

    /// An arrow function that fails and passes it up with `?`, or keys holding floats.
    fn arrows_or_floats(&mut self, body: &mut Vec<Stmt>) {
        if self.choice(2) == 0 {
            let limit = i64::from(self.byte() % 12) - 3;
            let how = match self.choice(6) {
                0 => Arrow::Tried,
                1 => Arrow::Twice,
                2 => Arrow::Each(self.of(Ty::Ints, 1)),
                3 => Arrow::RanFine,
                4 => Arrow::Fallback,
                _ => Arrow::Either(self.int(1)),
            };
            let n = self.int(1);
            body.push(Stmt::Arrowed(how, n, limit));
            return;
        }
        let held = match self.choice(5) {
            0 => FloatHeld::Gauge,
            1 => FloatHeld::Gauge32,
            2 => FloatHeld::Level,
            3 => FloatHeld::Level32,
            _ => FloatHeld::Marked,
        };
        // `Off` is only an enum's.
        let kinds = if matches!(held, FloatHeld::Level | FloatHeld::Level32) {
            FLOATS.len()
        } else {
            FLOATS.len() - 1
        };
        let count = 2 + self.choice(4);
        let values: Vec<usize> = (0..count).map(|_| self.choice(kinds)).collect();
        let probe = self.choice(count);
        let asked = self.choice(kinds);
        let pair = (self.choice(count), self.choice(count));
        let names = (self.fresh(), self.fresh());
        body.push(Stmt::FloatKeys { held, names, values, probe, asked, pair });
    }

    fn lent_or_self(&mut self, body: &mut Vec<Stmt>) {
        /// The types of locals that can be assigned to themselves: not `shared`, nor reached
        /// only through what lends it.
        const PLAIN: [Ty; 12] = [
            Ty::Int,
            Ty::Str,
            Ty::Ints,
            Ty::Point,
            Ty::Shape,
            Ty::Holder,
            Ty::Lists,
            Ty::Counts,
            Ty::Buf,
            Ty::Node,
            Ty::Nodes,
            Ty::Named,
        ];
        match self.choice(4) {
            0 => {
                let Some(list) = self.name_of(Ty::Ints, true) else { return self.print(body) };
                body.push(Stmt::LentRef(list, self.fresh()));
            }
            1 => {
                let items = self.of(Ty::Ints, 1);
                body.push(Stmt::LentItem(self.fresh(), self.fresh(), items));
            }
            2 => {
                let Some(holder) = self.name_of(Ty::Holder, true) else { return self.print(body) };
                body.push(Stmt::SelfField(holder.clone()));
                self.read_back(body, &holder);
            }
            _ => {
                let ty = PLAIN.get(self.choice(PLAIN.len())).copied().unwrap_or(Ty::Int);
                let Some(name) = self.name_of(ty, true) else { return self.print(body) };
                body.push(Stmt::Assign {
                    place: Place::Name(name.clone()),
                    value: Expr::Name(name.clone()),
                    add: false,
                });
                self.read_back(body, &name);
            }
        }
    }

    /// What copies are tested on: a function capturing a list that can't change, while one that
    /// shares its value can; a struct holding a list, changed; and lists stored into a dict.
    fn copies(&mut self, body: &mut Vec<Stmt>, which: usize) {
        match which {
            23 if !self.threadless && self.choice(3) == 0 => {
                // Collect on the threads, then apply in order.
                const LABELS: [&str; 4] = ["a", "b", "hé🦊", ""];
                let count = 1 + self.choice(LABELS.len());
                let mut spots = Vec::with_capacity(count);
                for label in LABELS.iter().take(count) {
                    let x = self.int(2);
                    spots.push((*label, self.bounded(x)));
                }
                let xs = self.of(Ty::Ints, 1);
                let (world, changes) = (self.fresh(), self.fresh());
                body.push(Stmt::PlanApply(world, spots, xs, changes));
            }
            23 if self.choice(4) == 0 => {
                // Locals named like functions that take a `shared` value.
                self.made += 1;
                let by = self.int(1);
                body.push(Stmt::Shadowed(self.made, self.bounded(by)));
            }
            23 if self.choice(3) == 0 => {
                // A union's member called on through the union: after `match`, after `is`,
                // straight on it, with its `with` source after `is` or straight on it too, or on
                // what's left of three after `is`.
                let how = [Via::Match, Via::Is, Via::Direct, Via::Rest, Via::Plain]
                    .get(self.choice(5))
                    .copied()
                    .unwrap_or(Via::Match);
                let members = if matches!(how, Via::Rest) { 3 } else { 2 + self.choice(2) };
                let member = self.choice(members);
                let text = self.of(Ty::Str, 2);
                body.push(Stmt::UnionCall(how, members, member, text));
            }
            23 => {
                let fixed: Vec<String> = self
                    .names
                    .iter()
                    .filter(|name| name.ty == Ty::Ints && !name.changeable)
                    .map(|name| name.name.clone())
                    .collect();
                let Some(captured) = fixed.get(self.choice(fixed.len())).cloned() else {
                    return self.print(body);
                };
                let name = self.fresh();
                body.push(Stmt::Let {
                    name: name.clone(),
                    value: Expr::Summing(captured),
                    changeable: false,
                });
                self.names.push(InScope { name, ty: Ty::Count, changeable: false, held: 0 });
            }
            25 if self.choice(3) == 0 => self.lent_or_self(body),
            25 if self.choice(3) == 0 => {
                // A container of containers copied, then changed deep inside through the copy.
                const SHAPES: [Nested; 8] = [
                    Nested::DictOfUnits,
                    Nested::ArmyOfUnits,
                    Nested::DictOfDicts,
                    Nested::ListOfDicts,
                    Nested::DictOfUnitLists,
                    Nested::DictOfUnitsHp,
                    Nested::DictOfIntLists,
                    Nested::ReplacedInCopy,
                ];
                let shape = SHAPES.get(self.choice(SHAPES.len())).copied().unwrap_or(Nested::DictOfUnits);
                let (given, copy) = (self.fresh(), self.fresh());
                let value = self.int(1);
                let value = self.bounded(value);
                body.push(Stmt::CopiedNested(shape, given, copy, value));
            }
            24 if self.choice(3) == 0 => {
                let Some(holder) = self.name_of(Ty::Holder, true) else { return self.print(body) };
                body.push(Stmt::GrownBy(holder, self.fresh()));
            }
            24 => {
                let Some(holder) = self.name_of(Ty::Holder, true) else { return self.print(body) };
                let value = self.int(1);
                let value = self.bounded(value);
                body.push(if self.choice(2) == 0 {
                    Stmt::HolderAppend(holder.clone(), value)
                } else {
                    Stmt::HolderTag(holder.clone(), value)
                });
                self.read_back(body, &holder);
            }
            _ => {
                let Some(lists) = self.name_of(Ty::Lists, true) else { return self.print(body) };
                let key = KEYS.get(self.choice(KEYS.len())).copied().unwrap_or("a");
                if self.choice(2) == 0 {
                    let list = self.of(Ty::Ints, 1);
                    body.push(Stmt::PutList(lists.clone(), key, list));
                } else {
                    let value = self.int(1);
                    body.push(Stmt::ListAppend(lists.clone(), key, value));
                }
                self.read_back(body, &lists);
            }
        }
    }

    /// A change to a `dict[str, int]`: a key set, added to or removed.
    fn change_counts(&mut self, body: &mut Vec<Stmt>) {
        let Some(counts) = self.name_of(Ty::Counts, true) else { return self.print(body) };
        let key = KEYS.get(self.choice(KEYS.len())).copied().unwrap_or("a");
        match self.choice(3) {
            0 => body.push(Stmt::RemoveKey(counts.clone(), key)),
            1 => {
                let value = self.int(0);
                let value = self.bounded(value);
                body.push(Stmt::SetKey(counts.clone(), key, value, false));
            }
            _ => {
                let value = self.int(1);
                let value = self.bounded(value);
                let add = Stmt::SetKey(counts.clone(), key, value, true);
                // A key that isn't there fails `+=`, which only a risky program risks.
                if self.risky {
                    body.push(add);
                } else {
                    let there = Expr::Has(Box::new(Expr::Name(counts.clone())), key);
                    body.push(Stmt::If(there, vec![add], Vec::new()));
                }
            }
        }
        self.read_back(body, &counts);
    }

    /// A change to a `Buffer[int]`, kept from failing unless the program is risky.
    fn change_buffer(&mut self, body: &mut Vec<Stmt>) {
        let Some(buffer) = self.name_of(Ty::Buf, true) else { return self.print(body) };
        let length = Expr::BufLen(Box::new(Expr::Name(buffer.clone())));
        let any = Expr::Compare(Compare::Greater, Box::new(length.clone()), Box::new(Expr::Int(0)));
        // A place in the buffer: as it comes in a risky program, else kept inside it, where
        // there's anything.
        let at = self.int(2);
        let inside = Expr::Arith(Arith::Rem, Box::new(at.clone()), Box::new(length.clone()));
        let at = if self.risky { at } else { inside };
        let guarded = |risky: bool, op: BufOp| {
            let change = Stmt::Buffered(buffer.clone(), op);
            if risky { change } else { Stmt::If(any.clone(), vec![change], Vec::new()) }
        };
        let change = match self.choice(7) {
            0 | 1 => Stmt::Buffered(buffer.clone(), BufOp::Push(self.int(1))),
            2 => guarded(self.risky, BufOp::Pop),
            3 => {
                let value = self.int(1);
                guarded(self.risky, BufOp::Set(at, value))
            }
            4 => {
                let place = if self.risky {
                    self.int(2)
                } else if self.choice(2) == 0 {
                    length
                } else {
                    Expr::Int(0)
                };
                Stmt::Buffered(buffer.clone(), BufOp::Insert(place, self.int(1)))
            }
            5 => guarded(self.risky, BufOp::Remove(at)),
            _ => {
                let others: Vec<String> = self
                    .names
                    .iter()
                    .filter(|name| name.ty == Ty::Buf && name.name != buffer)
                    .map(|name| name.name.clone())
                    .collect();
                match others.get(self.choice(others.len().max(1))) {
                    Some(other) => Stmt::Buffered(buffer.clone(), BufOp::Extend(other.clone())),
                    None => {
                        Stmt::Buffered(buffer.clone(), BufOp::Truncate(Expr::Int(i64::from(self.byte() % 4))))
                    }
                }
            }
        };
        body.push(change);
        self.read_back(body, &buffer);
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
            let value = printable(Expr::Name(found.name.clone()), found.ty, self.digest_only);
            body.push(Stmt::Print(vec![(value, found.ty)]));
        }
    }

    fn print(&mut self, body: &mut Vec<Stmt>) {
        let ty = self.ty();
        let value = self.of(ty, 0);
        body.push(Stmt::Print(vec![(printable(value, ty, self.digest_only), ty)]));
    }

    /// A change to a tree or a list of them, a deep tree, one built in a loop that can fail
    /// partway, or a copy of one kept while the first changes.
    fn trees(&mut self, body: &mut Vec<Stmt>) {
        match self.choice(7) {
            0 => {
                let Some(nodes) = self.name_of(Ty::Nodes, true) else { return self.print(body) };
                let node = self.node_for_list();
                body.push(Stmt::NodesAppend(nodes.clone(), node));
                self.read_back(body, &nodes);
            }
            1 => {
                let Some(nodes) = self.name_of(Ty::Nodes, true) else { return self.print(body) };
                body.push(Stmt::NodesPop(nodes.clone()));
                self.read_back(body, &nodes);
            }
            2 => {
                let Some(nodes) = self.name_of(Ty::Nodes, true) else { return self.print(body) };
                let at = self.int(1);
                let node = self.node_for_list();
                body.push(Stmt::NodesSet(nodes.clone(), at, node));
                self.read_back(body, &nodes);
            }
            3 => {
                let Some(node) = self.name_of(Ty::Node, true) else { return self.print(body) };
                self.reshape(body, &node);
            }
            4 => {
                // Deep enough to need a loop to let go of, where nothing else is repeating it.
                const DEPTHS: [i64; 7] = [0, 1, 2, 64, 1000, 20_000, 200_000];
                let most = if self.loops > 0 { 5 } else { DEPTHS.len() };
                let depth = DEPTHS.get(self.choice(most)).copied().unwrap_or(0);
                let label = self.of(Ty::Str, 2);
                let name = self.fresh();
                body.push(Stmt::Let {
                    name: name.clone(),
                    value: Expr::Deep(depth, Box::new(label)),
                    changeable: false,
                });
                self.names.push(InScope { name, ty: Ty::Deep, changeable: false, held: 0 });
            }
            5 => {
                // Built a branch a round, failing at round `fail` if the loop gets that far.
                let (name, round) = (self.fresh(), self.fresh());
                let count = 1 + i64::from(self.byte() % 6);
                let fail = i64::from(self.byte() % 16);
                let left =
                    Expr::Arith(Arith::Sub, Box::new(Expr::Int(fail)), Box::new(Expr::Name(round.clone())));
                let number =
                    Expr::Number(Box::new(Expr::Arith(Arith::Div, Box::new(Expr::Int(100)), Box::new(left))));
                let label = Expr::Format(Box::new(Expr::Name(round.clone())));
                let branch = Expr::Branch(Box::new(label), Box::new(Expr::NodeList(vec![number])));
                body.push(Stmt::Let {
                    name: name.clone(),
                    value: Expr::NodeList(Vec::new()),
                    changeable: true,
                });
                body.push(Stmt::Range(
                    round,
                    Expr::Int(count),
                    vec![Stmt::NodesAppend(name.clone(), branch)],
                ));
                self.names.push(InScope { name: name.clone(), ty: Ty::Nodes, changeable: true, held: 0 });
                self.read_back(body, &name);
            }
            _ => {
                // A copy kept while the first changes, which it mustn't see.
                let ty = if self.choice(2) == 0 { Ty::Node } else { Ty::Nodes };
                let Some(first) = self.name_of(ty, true) else { return self.print(body) };
                let kept = self.fresh();
                let value = Expr::Name(first.clone());
                body.push(Stmt::Let { name: kept.clone(), value, changeable: false });
                self.names.push(InScope { name: kept.clone(), ty, changeable: false, held: 0 });
                if ty == Ty::Node {
                    self.reshape(body, &first);
                } else {
                    let node = self.of(Ty::Node, 1);
                    body.push(Stmt::NodesAppend(first.clone(), node));
                    self.read_back(body, &first);
                }
                self.read_back(body, &kept);
            }
        }
    }

    /// One local `ref` or two in a row into the same container, the second taken after the
    /// first's last use, each changing its place and reading the container while it lives; then
    /// now and then a change to the container itself.
    fn local_refs(&mut self, body: &mut Vec<Stmt>) {
        const KINDS: [Ty; 5] = [Ty::Ints, Ty::Point, Ty::Holder, Ty::Holder, Ty::Nodes];
        let ty = KINDS.get(self.choice(KINDS.len())).copied().unwrap_or(Ty::Ints);
        let Some(container) = self.name_of(ty, true) else { return self.print(body) };
        for _ in 0..=self.choice(2) {
            self.local_ref(body, &container, ty);
        }
        if ty == Ty::Ints && self.choice(2) == 0 {
            let value = self.int(1);
            body.push(Stmt::Append(container.clone(), value));
        }
        self.read_back(body, &container);
    }

    /// `ref name = place` into `container`, and what uses it.
    fn local_ref(&mut self, body: &mut Vec<Stmt>, container: &str, ty: Ty) {
        let name = self.fresh();
        let held = Expr::Name(container.to_owned());
        let (place, named, length) = match ty {
            Ty::Point => (RefPlace::Field(container.to_owned(), self.choice(2) == 0), Ty::Int, None),
            Ty::Holder => match self.choice(3) {
                0 => (RefPlace::Tag(container.to_owned()), Ty::Int, None),
                1 => (RefPlace::HeldList(container.to_owned()), Ty::Ints, None),
                _ => {
                    let length = Expr::Len(Box::new(Expr::HeldItems(Box::new(held))));
                    (RefPlace::HeldItem(container.to_owned(), self.int(2)), Ty::Int, Some(length))
                }
            },
            Ty::Nodes => {
                let length = Expr::Len(Box::new(held));
                (RefPlace::NodeItem(container.to_owned(), self.int(2)), Ty::Node, Some(length))
            }
            _ => {
                (RefPlace::Item(container.to_owned(), self.int(2)), Ty::Int, Some(Expr::Len(Box::new(held))))
            }
        };
        let mut uses = Vec::new();
        for _ in 0..self.choice(3) {
            let read = Expr::Name(name.clone());
            uses.push(match (named, self.choice(3)) {
                (Ty::Int, 0) => {
                    let by = self.int(1);
                    Stmt::Print(vec![(Expr::Bump(name.clone(), Box::new(self.bounded(by))), Ty::Int)])
                }
                (Ty::Int, _) => {
                    let value = self.int(1);
                    let value = self.bounded(value);
                    Stmt::Assign { place: Place::Name(name.clone()), value, add: self.choice(2) == 0 }
                }
                (Ty::Ints, 0) => Stmt::Pop(name.clone()),
                (Ty::Ints, _) => Stmt::Append(name.clone(), self.int(1)),
                (_, _) => {
                    let text = self.of(Ty::Str, 2);
                    self.tree_names = 0;
                    let child = self.of(Ty::Node, 1);
                    Stmt::Reshape(name.clone(), text, child)
                }
            });
            // Read while the `ref` lives: it and the container it points into, which sees the change.
            if self.choice(2) == 0 {
                uses.push(Stmt::Print(vec![
                    (printable(read, named, self.digest_only), named),
                    (printable(Expr::Name(container.to_owned()), ty, self.digest_only), ty),
                ]));
            }
        }
        let read = printable(Expr::Name(name.clone()), named, self.digest_only);
        uses.push(Stmt::Print(vec![(read, named)]));
        let Some(length) = length.filter(|_| !self.risky) else {
            body.push(Stmt::LocalRef(name, place, uses));
            return;
        };
        // Kept inside the list unless the program is risky, where a place outside it fails.
        let inside = |at: Expr| Expr::Arith(Arith::Rem, Box::new(at), Box::new(length.clone()));
        let place = match place {
            RefPlace::Item(list, at) => RefPlace::Item(list, inside(at)),
            RefPlace::HeldItem(holder, at) => RefPlace::HeldItem(holder, inside(at)),
            RefPlace::NodeItem(nodes, at) => RefPlace::NodeItem(nodes, inside(at)),
            place => place,
        };
        let any = Expr::Compare(Compare::Greater, Box::new(length.clone()), Box::new(Expr::Int(0)));
        body.push(Stmt::If(any, vec![Stmt::LocalRef(name, place, uses)], Vec::new()));
    }

    /// A change to a `Named`, a deep one, one built in a loop that can fail partway, or a copy
    /// of one kept while the first changes.
    fn named(&mut self, body: &mut Vec<Stmt>) {
        match self.choice(4) {
            0 => {
                let Some(named) = self.name_of(Ty::Named, true) else { return self.print(body) };
                self.rename(body, &named);
            }
            1 => {
                const DEPTHS: [i64; 5] = [65, 1000, 5000, 20_000, 200_000];
                let most = if self.loops > 0 { 2 } else { DEPTHS.len() };
                let depth = DEPTHS.get(self.choice(most)).copied().unwrap_or(65);
                let label = self.of(Ty::Str, 2);
                let name = self.fresh();
                body.push(Stmt::Let {
                    name: name.clone(),
                    value: Expr::Chain(depth, Box::new(label)),
                    changeable: false,
                });
                self.names.push(InScope { name, ty: Ty::Chain, changeable: false, held: 0 });
            }
            2 => {
                // Built a pair a round, failing at round `fail` if the loop gets that far.
                let (name, round) = (self.fresh(), self.fresh());
                let count = 1 + i64::from(self.byte() % 6);
                let fail = i64::from(self.byte() % 16);
                let left =
                    Expr::Arith(Arith::Sub, Box::new(Expr::Int(fail)), Box::new(Expr::Name(round.clone())));
                let number = Expr::Arith(Arith::Div, Box::new(Expr::Int(100)), Box::new(left));
                let right = Expr::NamedLeaf(Box::new(Expr::Format(Box::new(number))));
                let grown = Expr::NamedPair(
                    Box::new(Expr::Name(name.clone())),
                    Box::new(right),
                    Box::new(Expr::Format(Box::new(Expr::Name(round.clone())))),
                    Box::new(Expr::Texts(Vec::new())),
                );
                let start = Expr::NamedLeaf(Box::new(Expr::Str("s".to_owned())));
                body.push(Stmt::Let { name: name.clone(), value: start, changeable: true });
                let grow = Stmt::Assign { place: Place::Name(name.clone()), value: grown, add: false };
                body.push(Stmt::Range(round, Expr::Int(count), vec![grow]));
                self.names.push(InScope { name: name.clone(), ty: Ty::Named, changeable: true, held: 0 });
                self.read_back(body, &name);
            }
            _ => {
                // A copy kept while the first changes, which it mustn't see.
                let Some(first) = self.name_of(Ty::Named, true) else { return self.print(body) };
                let kept = self.fresh();
                body.push(Stmt::Let {
                    name: kept.clone(),
                    value: Expr::Name(first.clone()),
                    changeable: false,
                });
                self.names.push(InScope { name: kept.clone(), ty: Ty::Named, changeable: false, held: 0 });
                self.rename(body, &first);
                self.read_back(body, &kept);
            }
        }
    }

    /// A node to append to a list of them or to put in one's place. Inside a loop it holds no
    /// tree's name: one holding the list itself, as in `nodes.append(.Branch(label="a",
    /// children=nodes))`, doubles the list every round, and loops inside loops make it too big
    /// for any machine.
    fn node_for_list(&mut self) -> Expr {
        if self.loops > 0 {
            self.tree_names = 0;
        }
        self.of(Ty::Node, 1)
    }

    /// `match named:` changing it through `ref`, with a new left side that holds no tree's name.
    fn rename(&mut self, body: &mut Vec<Stmt>, named: &str) {
        let text = self.of(Ty::Str, 2);
        self.tree_names = 0;
        let left = self.of(Ty::Named, 1);
        body.push(Stmt::Rename(named.to_owned(), text, left));
        self.read_back(body, named);
    }

    /// `match node:` changing it through `ref`, with a child that holds no other tree's name.
    fn reshape(&mut self, body: &mut Vec<Stmt>, node: &str) {
        let text = self.of(Ty::Str, 2);
        self.tree_names = 0;
        let child = self.of(Ty::Node, 1);
        body.push(Stmt::Reshape(node.to_owned(), text, child));
        self.read_back(body, node);
    }
}
