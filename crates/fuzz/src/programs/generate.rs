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

use super::ast::{ARRAY_WAYS, ArrayStep, ArrayWay, Arraying};
use super::ast::{
    Alias, Aliasing, Amount, Arith, Arrow, BufOp, Case, Compare, Constants, Expr, FLOATS, FloatHeld, Held,
    Leave, Nested, Op, Place, Program, RefPlace, STARTING, Size, Stmt, StreamMap, StreamRead, Streaming,
    TYPES, Turn, Turning, Ty, Via, Writing,
};
use super::ast::{Between, Looping, Reading, loops_back};
use super::ast::{
    CAPTURE_WAYS, Calling, CaptureWay, Capturing, Field, FieldCall, FieldCalls, Listening, REF_STARTS,
    REF_TEXTS, REF_WAYS, RefCasing, RefWay, SWAP_KEYS, ShelfPlace, Shelve, Shelving, SwapField, Swapping,
    Waking, Yielding, ZIP_WAYS, ZipSumming, ZipWay,
};
use super::ast::{EXCHANGE_WAYS, ExchangeStep, ExchangeWay, Exchanging};
use super::ast::{FILE_PIECES, FILE_WAYS, FileStep, FileWay, Filing};
use super::ast::{GEN_WAYS, GenStep, GenWay, Generating};
use super::ast::{GENERIC_STEPS, GENERIC_TYS, GenericStep, GenericTy, Genericking};
use super::ast::{IFACE_STEPS, IfaceStep, Interfacing};
use super::ast::{InOrder, ORDER_STEPS, OrderStep};
use super::ast::{LOCK_WAYS, LockStep, LockWay, LockingTogether};
use super::ast::{NARROW_WAYS, NarrowWay, Narrowing};
use super::ast::{OWN_STEPS, OwnIndexing, OwnStep};
use super::ast::{POP_TYS, POP_WAYS, PopTy, PopWay, Popping};
use super::ast::{RULE_WAYS, RuleStep, RuleWay, Ruling};
use super::ast::{SIZE_NUMS, SIZED_WAYS, SizeNum, SizedWay, Sizing, sized_choice, sized_step};
use super::ast::{STUCK_WAYS, Sticking, StuckWay};

/// Whether random programs loop over `enumerate` or `zip` of a generator made right there, lending
/// from a source or failing partway (`ZipSumming`): the loop owns it since #2041.
const ZIP_GENERATORS: bool = true;

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

/// Whether random programs write files in a folder of their own (`Filed`): through `with
/// fs.create(...)` failing, passing a failure up with `?` or crashing partway, which leaves the old
/// file byte for byte, or ending well, which replaces it whole; two files in one `with`; a plain
/// write; and an `Fs` narrowed with `read_only` or `only` refusing. The second row of the coverage
/// map: nothing wrote a file before.
const FILE_WRITES: bool = true;

/// Whether random programs give values of one type, an int, a text, a list or a struct holding a
/// list, to a generic holder and generic functions (`Generic`): `ref self` and `ref T` changes,
/// copies made inside generic code, a holder in a holder, in a list and in a dict. Map row 4:
/// nothing generic was written before.
const GENERICS: bool = true;

/// Whether random programs wait on what nothing will end (`Stuck`): with a time limit, which times
/// out, and without, a deadlock, which stops the program. Map row 3: a hang in production is a
/// blow-up. On since the oracle stops a deadlock the runtime finds, rather than refusing it.
const DEADLOCKS: bool = true;

/// Whether random programs give two types with one interface to generic functions bounded by it
/// (`Interfaced`): `ref self` methods through `ref G` and `for ref`, a copy grown inside generic
/// code, a method that fails partway passed up with `?`, and one that's a generator. Map row 5:
/// nothing wrote an interface before.
const INTERFACES: bool = true;

/// Whether random programs take two `shared` values in one `lock` (`LockedTogether`): both
/// changed, then a `fail` or a crash in a task putting both back, both read, and one changed from
/// what the other holds. Map row 4: every `lock` named one value before.
const LOCKED_TOGETHER: bool = true;

/// Whether a `LockedTogether` statement's `lock` may name its two values against the order they
/// were made. A crash there once left their lists reading as `None` after the rollback (#2223).
const LOCKED_AGAINST_MADE_ORDER: bool = true;

/// Whether random programs hold fixed-size arrays (`Arrayed`): of ints, of structs holding a list,
/// of optional structs, of texts and of arrays, in a struct held in a list, a Table and a dict,
/// changed through each, by `for ref` and by `ref`, copied by `=` and changed apart, and indexed
/// past either end where the proofs can't see the bound. Map row 6: nothing wrote an array before.
const ARRAYS: bool = true;

/// Whether an `Arrayed` statement changes a field of an optional struct in an array once
/// `is not None` narrows it, in a `for ref` over the array. On since #2266 lowered it (#2255).
pub(super) const NARROWED_FIELD_WRITES: bool = true;

/// Whether random programs write fields with `where` rules every way (`Ruled`): set, added to,
/// taken from, through a `ref self` method, lent alone by `ref` to a call and to a `for ref`
/// loop's turn, and dipped below and put back before it comes back, which is fine (#2270); the
/// whole struct lent by `ref`; in a list and a dict; `copy`; and a value made from a number worked
/// out as it runs, which is refused when it breaks one. A risky program breaks one, which stops
/// it. Map row 9: nothing wrote a `where` rule before.
const WHERE_RULES: bool = true;

/// Whether random programs loop over generators that fail partway (`Generated`): wrapped with
/// `? from` in a `for`, in `enumerate` and in `zip` on either side or both, and held in a name, a
/// generator kept for later, a tuple, a list and a generic struct, each looped over where it's
/// made. Map row 7: nothing wrote `? from` before, and the oracle runs `zip` wrapped two ways now.
const FAILING_GENERATORS: bool = true;

/// Whether random programs put optionals of a heap type in a list and take them out every way
/// (`Popped`): `pop()`, `pop(at)`, `remove`, `discard` and through `ref`, kept, inline and
/// dropped, from a local and a struct's field. On since popping a present `str?` out of a
/// `list[str?]` stopped crashing native code (#2282, fixed by #2292).
const POPPED_OPTIONALS: bool = true;

/// Whether random programs swap a list's items at places counting from either end, directly and
/// through a `ref` parameter, and update a Table's rows at live and removed `Id`s (`Exchanged`).
/// Map row 15: nothing called `swap` or `Table.update` before.
const EXCHANGED: bool = true;

/// Whether a statement saves a box of the program's own values and loads it back (`Saved`):
/// unions, an enum, a `where` rule, two `Table`s with `Id`s into them and optionals, through JSON,
/// `std.pack` (in order and exact) and TOML, between steps changing it, and texts that don't hold
/// one refused.
const SAVED_VALUES: bool = true;

/// Whether a statement keeps rows in a SQLite database through `std.sqlite` (`Stored`): rows put
/// in, changed and taken out, some breaking the table's rules, read back as structs, a column and
/// a sum, and transactions kept or undone while the database outside them sees what was kept.
const STORED_ROWS: bool = true;

/// Whether a statement has tasks change a `shared Bag` under its lock (`Crewed`), one task after
/// another, each taking every number it's sent through two or three locks: appends and pops,
/// entries set, removed and popped, and `ref self` calls with arguments worked out from the number
/// and the bag, any of which can crash partway through a lock. `main` locks and reads the bag after
/// each task. #2516's segfault, at `main`'s next lock after such a crash, was found only by chance.
const LOCKED_CREWS: bool = true;

/// Whether a statement takes links from a `std.concurrent` pool (`Pooled`): `main` taking them
/// nested up to the pool's size, blocks failing, a task crashing with one, a short wait timing out
/// where every link is held, an open refused, and links that can't be reset replaced. Pools had no
/// shape before.
const KEEPER_POOLS: bool = true;

/// Whether a statement sends enums to a task (`Messaged`): flat ones, which go as words once enums
/// do (cheap messages, step 5), and ones that aren't, which are copied as blocks, each case's texts
/// short, long, empty or the sender's own as it changes.
const ENUM_MESSAGES: bool = true;

/// Whether a statement opens two or three handles in one `with` (`Opened`): left to right, one
/// perhaps failing to open, a lent pair taken apart inside, and the block ending, failing or
/// crashing, with cleanups that may crash.
const OPENED_HANDLES: bool = true;

/// Whether random programs change a number of each of the eight integer sizes every way
/// (`Sized`): checked arithmetic, `wrapping_` and `saturating_`, shifts past the top and at the
/// width, bits, `sum`, widening, `parse`, and a loop whose bound the proofs can't see past. A
/// risky program now and then overflows, shifts at the width or divides by zero, which stops
/// it. Map row 10: only `u8`, `i32` and `u64` were written before, and `<<` never.
const SIZED_NUMBERS: bool = true;

/// Whether `int` has `wrapping_` and `saturating_` methods, as every other size does (#2394, since
/// #2405). Without them, an `i64` statement writes a checked one in their place.
const INT_WRAPPING_METHODS: bool = true;

/// Whether random programs read what a later call in the same line changes: receivers, an index's
/// base, operators, arguments and writes, each read left to right (`Ordered`). On since compiled
/// code and the oracle both read a receiver before its arguments (#546).
const EVALUATION_ORDER: bool = true;

/// Whether random programs change values of types with their own `def []` through it (`Owned`):
/// the plain form beside a `def []=`, nested, failing partway and given by `ref`, every built-in
/// method that changes a list or only reads it, and the `ref` form's places reached by `=`, `ref`
/// and `for ref`. On since the oracle writes back every change through it (#2192).
const OWN_INDEXES: bool = true;

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

/// Whether random programs listen to a channel to every listener, through inboxes read one
/// message at a time or in a loop, and through a loop that drops the oldest of what it falls
/// behind on, with inboxes falling behind loudly, as #1799 decided.
const EVERY_LISTENER: bool = true;

/// Whether random programs keep what a lookup in a struct's dict found, set the struct whole to
/// another, then store at the same key: the store has to go to the dict the struct holds now.
/// Main stored into the dict the lookup searched, the caller's or one let go of, as the review
/// of #1785 found (finding 3), and this shape found it within a few dozen programs. On since
/// #1812 fixed it; the seed `review_1785_lookup_swapped` is a program that failed before.
const LOOKUPS_SWAPPED: bool = true;

/// Whether random programs write a `match` with `ref` each way the compiled program put back wrong
/// or couldn't build (`RefWay`): three `match`es deep, a whole new value through a pattern naming
/// its fields out of order, a `ref` name a parameter has too, a guard reading a `ref` name, the
/// index of what's matched changed in the case, and a `for ref` in the case whose item's change a
/// `?` failing in a `while` condition, a later `elif` or after an `and` lost (#2025). The oracle
/// found them, held to the compiled program. On since #2017 put each case back by its fields'
/// names, and #2038 wrote a `for ref` item back before its case.
const REF_CASES: bool = true;

/// Whether random programs narrow a union of two structs to one member and change it through a
/// `ref self` method, a `match` case, a `for ref` loop, a field, or a `ref` call taking the member or
/// the union (`Narrowed`). The method was given the whole union, so compiled code crashed (#2157).
const NARROWED_UNIONS: bool = true;

/// Whether a `Narrowed` statement grows a name narrowed by what it was given right after a `ref`
/// call that gave it the other member (`NarrowWay::RefUnionGrown`). The checker kept the narrowing,
/// so compiled code ran the other member's method on it (#2163). On since #2177 ended the
/// narrowing at the call.
const REF_ENDS_NARROWING: bool = true;

/// Whether random programs write a name where another of the same name and type is in sight, each
/// way the checker renamed by text rather than by what the name means where it's written
/// (`CaptureWay`): a nested function's parameter under `with ... as` that name, a loop's, a
/// case's, a nested function's or an arrow's name under `lock ... as ref` that name, and a `with`
/// source calling a function its caller has a same-typed parameter named as (#1997). The checker
/// lane's sweep found them, each a silent wrong answer, or a refusal or crash where none is due.
/// On since #2024 made every rewrite change only the uses in its sight, and `SourceCall` since #2051
/// kept a source's own function its own. `LockDef` isn't written yet: the checker refuses it as
/// taking the lock until #2011, which is lock order, not capture.
const NAME_CAPTURE: bool = true;

/// Whether random programs crash inside a `ref self` method, or a change by hand, on part of a
/// list or dict: under a `shared` value's lock, which has to undo the whole lock, or held by the
/// task alone, which has to let go of all of it. Main put back a freed block when the part was a
/// list's item under a lock, so the pouch printed `items=None` and the program stopped with "a
/// list native code changed isn't one this thread can find", as the review of #1785 found
/// (finding 1, `b5.fen`). With no lock, it left the blocks the task made live. On since #1853
/// fixed both; the seed `review_1785_crash_in_locked_part` is a program that failed before.
const PART_CRASHES: bool = true;

/// Whether a call handling several of `Crashed`, `Closed`, `Timeout` and `Refused` prints each
/// one's own word, `crashed`, `closed`, `timeout` or `refused`, rather than one word for them all,
/// so a task's failure that comes out as the wrong one shows. On since a launched task that fails
/// while its block still sends ends the block with its own failure rather than `Closed` (#1724).
pub(crate) const DISTINCT_STOPS: bool = true;

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
                let list = self.long_or(next);
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
            // A sum over `parallel`, added in the runtime's tree, or in order with an `if`.
            5 if !self.threadless && self.choice(2) == 0 => {
                let list = self.long_or(next);
                let filtered = self.choice(2) == 0;
                Expr::FloatSum(Box::new(list), Box::new(self.float(next)), filtered)
            }
            _ => self.name_of(Ty::Float, false).map_or_else(|| literal(self.choice(9), &FLOATS), Expr::Name),
        }
    }

    /// A list of ints for a `parallel` sum: a quarter of the time one longer than a leaf of its
    /// tree, of 1,024 positions, so the tree has halves to add, and otherwise any list.
    fn long_or(&mut self, depth: usize) -> Expr {
        const LONG: [usize; 3] = [1025, 2049, 3000];
        if self.choice(4) == 0 {
            Expr::Upto(LONG.get(self.choice(LONG.len())).copied().unwrap_or(1025))
        } else {
            self.of(Ty::Ints, depth)
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
            26 if LOOKUPS_SWAPPED && self.choice(4) == 0 => self.swaps(body),
            26 if PART_CRASHES && !self.threadless && self.choice(4) == 0 => self.shelves(body),
            26 if REF_CASES && self.choice(4) == 0 => self.ref_cases(body),
            26 if NAME_CAPTURE && self.choice(4) == 0 => self.captures(body),
            26 if EVALUATION_ORDER && self.choice(4) == 0 => self.in_order(body),
            26 if FILE_WRITES && self.choice(4) == 0 => self.files(body),
            26 if OPENED_HANDLES && self.choice(4) == 0 => self.opened(body),
            26 if ENUM_MESSAGES && !self.threadless && self.choice(4) == 0 => self.enum_messages(body),
            26 => self.change_counts(body),
            // The guarded shapes are tried first, as they were before the shapes after them, so
            // each of their cases still comes up as often.
            27 if self.choice(3) == 0 => self.guards(body),
            27 if CONSTANTS_IN_TASKS && !self.threadless && self.choice(4) == 0 => self.constants(body),
            27 if FIELD_CALLS && self.choice(4) == 0 => self.field_calls(body),
            27 if YIELDS && !self.threadless && self.choice(4) == 0 => self.yields(body),
            27 if CRASH_WAKES && !self.threadless && self.choice(4) == 0 => self.wakes(body),
            27 if EVERY_LISTENER && !self.threadless && self.choice(4) == 0 => self.listens(body),
            27 if NARROWED_UNIONS && self.choice(4) == 0 => self.narrows(body),
            27 if OWN_INDEXES && self.choice(4) == 0 => self.own_indexes(body),
            27 if GENERICS && self.choice(4) == 0 => self.generics(body),
            27 if DEADLOCKS && self.choice(4) == 0 => self.stucks(body),
            27 if INTERFACES && self.choice(4) == 0 => self.interfaces(body),
            27 if LOCKED_TOGETHER && self.choice(4) == 0 => self.locked_together(body),
            27 if ARRAYS && self.choice(4) == 0 => self.arrays(body),
            27 if SIZED_NUMBERS && self.choice(4) == 0 => self.sized_numbers(body),
            27 if FAILING_GENERATORS && self.choice(4) == 0 => self.failing_generators(body),
            27 if WHERE_RULES && self.choice(4) == 0 => self.where_rules(body),
            27 if POPPED_OPTIONALS && self.choice(4) == 0 => self.popped_optionals(body),
            27 if EXCHANGED && self.choice(4) == 0 => self.exchanged(body),
            27 if SAVED_VALUES && self.choice(4) == 0 => self.saved_values(body),
            27 if STORED_ROWS && self.choice(4) == 0 => self.stored_rows(body),
            27 if LOCKED_CREWS && !self.threadless && self.choice(4) == 0 => self.locked_crews(body),
            27 if KEEPER_POOLS && !self.threadless && self.choice(4) == 0 => self.keeper_pools(body),
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
            28 if ZIP_GENERATORS && self.choice(3) == 0 => {
                // A generator made right inside `enumerate` or `zip`, let go partway, run out, or
                // failing partway.
                let way = ZIP_WAYS.get(self.choice(ZIP_WAYS.len())).copied().unwrap_or(ZipWay::First);
                let failing = self.choice(2) == 0;
                let limit = i64::from(self.byte() % 6);
                let stop = i64::from(self.byte() % if failing { 7 } else { 10 });
                let words = i64::from(self.byte() % 6);
                body.push(Stmt::ZipSummed(ZipSumming { way, failing, limit, stop, words }));
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

    /// Listeners of a channel to every listener: inboxes, some falling behind, and a loop that
    /// drops the oldest.
    fn listens(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let size = 1 + i64::from(self.byte() % 4);
        let early = self.choice(2) == 0;
        let sent = 1 + i64::from(self.byte() % 6);
        let dropping = self.choice(2) == 0;
        let count = usize::from(!dropping) + self.choice(3 - usize::from(!dropping));
        let inboxes = (0..count).map(|_| self.choice(2) == 0).collect();
        body.push(Stmt::Listened(Listening { id, size, early, sent, dropping, inboxes }));
    }

    /// A lookup kept while the struct whose dict it searched is set to another, then a store at
    /// the same key.
    /// A `match` with `ref` written one of the ways the compiled program once got wrong.
    fn ref_cases(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let way = REF_WAYS.get(self.choice(REF_WAYS.len())).copied().unwrap_or(RefWay::Deep);
        let start = self.choice(REF_STARTS);
        let text = self.choice(REF_TEXTS.len());
        body.push(Stmt::RefCased(RefCasing { id, way, start, text }));
    }

    /// A union narrowed to one member and changed through it, one of `NARROW_WAYS`, starting
    /// from 0 to 9.
    fn narrows(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let mut way = NARROW_WAYS.get(self.choice(NARROW_WAYS.len())).copied().unwrap_or(NarrowWay::Given);
        if way == NarrowWay::RefUnionGrown && !REF_ENDS_NARROWING {
            way = NarrowWay::RefUnion;
        }
        let crate_first = self.choice(2) == 0;
        let n = i64::from(self.byte() % 10);
        body.push(Stmt::Narrowed(Narrowing { id, way, crate_first, n }));
    }

    /// A name written where another of its name and type is in sight, one of `CAPTURE_WAYS`.
    fn captures(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        // `LockDef` waits for #2011.
        let ways: Vec<CaptureWay> =
            CAPTURE_WAYS.into_iter().filter(|way| *way != CaptureWay::LockDef).collect();
        let way = ways.get(self.choice(ways.len())).copied().unwrap_or(CaptureWay::WithDef);
        let n = i64::from(self.byte() % 20);
        body.push(Stmt::Captured(Capturing { id, way, n }));
    }

    fn swaps(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let base = i64::from(self.byte() % 20);
        let other_base = 100 + i64::from(self.byte() % 20);
        let field = if self.choice(2) == 0 { SwapField::Lists } else { SwapField::Counts };
        let key = self.choice(SWAP_KEYS.len());
        let fresh = i64::from(self.byte() % 50);
        let called = self.choice(2) == 0;
        let kept = self.choice(2) == 0;
        let owned = self.choice(2) == 0;
        let rounds = if self.choice(4) == 0 { 20 } else { 1 };
        body.push(Stmt::Swapped(Swapping {
            id,
            base,
            other_base,
            field,
            key,
            fresh,
            called,
            kept,
            owned,
            rounds,
        }));
    }

    /// A pouch changed in a task, under a lock or in what the task holds alone, by a `ref self`
    /// method on it or by hand, crashing partway for a value past 1.
    fn shelves(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let in_list = self.choice(2) == 0;
        let seen = i64::from(self.byte() % 10);
        let mut steps = Vec::new();
        for _ in 0..2 + self.choice(4) {
            let picked = self.byte();
            let value = i64::from(picked % 10);
            // The byte's tens say whether a form that reaches the pouch through the list or dict
            // hands that list or dict to a function to do it.
            let handed = picked / 10 % 2 == 1;
            let how = match (self.choice(3), handed) {
                (0, _) => Shelve::Fill,
                (1, false) => Shelve::ByHand,
                (1, true) => Shelve::HandedByHand,
                (_, false) => Shelve::Direct,
                (_, true) => Shelve::Handed,
            };
            steps.push((value, how));
        }
        let place = match self.choice(3) {
            0 => ShelfPlace::Locked,
            1 => ShelfPlace::Owned,
            _ => ShelfPlace::Bare,
        };
        let rounds = 1 + i64::from(self.byte() % 6);
        body.push(Stmt::Shelved(Shelving { id, in_list, seen, place, rounds, steps }));
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
            if loops_back(kind) && self.choice(5) == 0 {
                let looping = self.looping(from, kind);
                steps.push(Alias::Looped(looping));
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

    /// A `Filed` statement: 2 to 8 steps, each writing 1 to 4 pieces and stopping at a piece from
    /// the first to one past the last, so some finish. A program without threads
    /// can't launch, so it saves plainly where it would in a task.
    fn files(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let mut steps = Vec::new();
        for _ in 0..2 + self.choice(7) {
            let mut way = FILE_WAYS.get(self.choice(FILE_WAYS.len())).copied().unwrap_or(FileWay::Saved);
            if way == FileWay::Tasked && self.threadless {
                way = FileWay::Saved;
            }
            let pieces: Vec<usize> = (0..=self.choice(4)).map(|_| self.choice(FILE_PIECES.len())).collect();
            let stop_at = self.choice(pieces.len() + 1);
            steps.push(FileStep { way, second: self.choice(2) == 0, pieces, stop_at });
        }
        body.push(Stmt::Filed(Filing { id, steps }));
    }

    /// An `Owned` statement: 3 to 10 steps, each at a position from 0 to 2 and with what it adds,
    /// from 0 to 9. A program without threads loops over the slots plainly where it would over
    /// `parallel`.
    fn own_indexes(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let base = i64::from(self.byte() % 10);
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let mut step = OWN_STEPS.get(self.choice(OWN_STEPS.len())).copied().unwrap_or(OwnStep::Plus);
            if step == OwnStep::ParallelRef && self.threadless {
                step = OwnStep::ForRef;
            }
            steps.push((step, self.choice(3), i64::from(self.byte() % 10)));
        }
        body.push(Stmt::Owned(OwnIndexing { id, base, steps }));
    }

    /// A `Generic` statement: one type, then 3 to 10 steps, each with a value and a count from 0
    /// to 2.
    fn generics(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let ty = GENERIC_TYS.get(self.choice(GENERIC_TYS.len())).copied().unwrap_or(GenericTy::Int);
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let step =
                GENERIC_STEPS.get(self.choice(GENERIC_STEPS.len())).copied().unwrap_or(GenericStep::Put);
            steps.push((step, self.choice(3), self.choice(3)));
        }
        body.push(Stmt::Generic(Genericking { id, ty, steps }));
    }

    /// A `Stuck` statement. A deadlock ends the program, so one in eight is one and the rest time
    /// out. A program without threads can't launch, or wait in rounds on other threads, so it
    /// receives plainly where it would.
    fn stucks(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let mut way = if self.choice(8) == 0 {
            STUCK_WAYS.get(2 + self.choice(4)).copied().unwrap_or(StuckWay::Received)
        } else {
            STUCK_WAYS.get(self.choice(2)).copied().unwrap_or(StuckWay::TimedReceive)
        };
        if self.threadless && matches!(way, StuckWay::BlockEnd | StuckWay::Rounds) {
            way = StuckWay::Received;
        }
        let n = if way == StuckWay::Rounds { 1 + self.choice(4) } else { 1 + self.choice(20) };
        body.push(Stmt::Stuck(Sticking { id, way, n: i64::try_from(n).unwrap_or(1) }));
    }

    /// An `Interfaced` statement: 3 to 10 steps, each on the crates or the heaps, adding 0 to 9.
    fn interfaces(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let step = IFACE_STEPS.get(self.choice(IFACE_STEPS.len())).copied().unwrap_or(IfaceStep::Bumped);
            steps.push((step, self.choice(2) == 0, i64::from(self.byte() % 10)));
        }
        body.push(Stmt::Interfaced(Interfacing { id, steps }));
    }

    /// A `LockedTogether` statement: 3 to 10 steps, each on two of the three accounts. A program
    /// without threads can't launch, so it moves plainly where it would crash in a task.
    fn locked_together(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let base = i64::from(self.byte() % 10);
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let mut way = LOCK_WAYS.get(self.choice(LOCK_WAYS.len())).copied().unwrap_or(LockWay::Moved);
            if way == LockWay::Crashed && self.threadless {
                way = LockWay::Moved;
            }
            let pair = [(0, 1), (0, 2), (1, 2)].get(self.choice(3)).copied().unwrap_or((0, 1));
            let amount = i64::from(self.byte() % 10);
            let backward = self.choice(2) == 0;
            let against = LOCKED_AGAINST_MADE_ORDER && self.choice(2) == 0;
            steps.push(LockStep { way, pair, amount, backward, against });
        }
        body.push(Stmt::LockedTogether(LockingTogether { id, base, steps }));
    }

    /// A `Ruled` statement: 3 to 10 steps, each with a value from 0 to 9. Only a risky program
    /// breaks a rule, and then a step in four does.
    fn where_rules(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let way = RULE_WAYS.get(self.choice(RULE_WAYS.len())).copied().unwrap_or(RuleWay::Raised);
            let value = i64::from(self.byte() % 10);
            let breaks = self.risky && self.choice(4) == 0;
            steps.push(RuleStep { way, value, breaks });
        }
        body.push(Stmt::Ruled(Ruling { id, steps }));
    }

    /// An `Exchanged` statement: one type, in a local or a field, then 3 to 14 steps, each with
    /// places from -4 to 3, a value from 0 to 2 and a row from 0 to 2.
    fn exchanged(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let ty = POP_TYS.get(self.choice(POP_TYS.len())).copied().unwrap_or(PopTy::Short);
        let in_field = self.choice(2) == 0;
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(12) {
            let way =
                EXCHANGE_WAYS.get(self.choice(EXCHANGE_WAYS.len())).copied().unwrap_or(ExchangeWay::Swap);
            let mut place = || i64::try_from(self.choice(8)).unwrap_or(0) - 4;
            let (first, second) = (place(), place());
            steps.push(ExchangeStep { way, first, second, which: self.choice(3), row: self.choice(3) });
        }
        body.push(Stmt::Exchanged(Exchanging { id, ty, in_field, steps }));
    }

    /// A `Popped` statement: one type, in a local or a field, then 3 to 12 steps, each with a
    /// value from 0 to 2.
    fn popped_optionals(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let ty = POP_TYS.get(self.choice(POP_TYS.len())).copied().unwrap_or(PopTy::Short);
        let in_field = self.choice(2) == 0;
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(10) {
            let way = POP_WAYS.get(self.choice(POP_WAYS.len())).copied().unwrap_or(PopWay::Kept);
            steps.push((way, self.choice(3)));
        }
        body.push(Stmt::Popped(Popping { id, ty, in_field, steps }));
    }

    /// An `Opened` statement: two or three handles, a quarter of them failing to open, and a
    /// crash, in a cleanup or the block, in one in ten, since it ends the program.
    fn opened(&mut self, body: &mut Vec<Stmt>) {
        use super::opening::{BlockEnd, Gate, Opening};
        self.made += 1;
        let id = self.made;
        let crashing = self.choice(10) == 0;
        let gates = (0..2 + self.choice(2))
            .map(|_| match self.choice(if crashing { 6 } else { 4 }) {
                0 => Gate::FailsOpen,
                4 => Gate::CrashesUndo,
                5 => Gate::CrashesClose,
                _ => Gate::Plain,
            })
            .collect();
        let pair = self.choice(2) == 0;
        let end = match self.choice(if crashing { 4 } else { 3 }) {
            0 => BlockEnd::Fails,
            3 => BlockEnd::Crashes,
            _ => BlockEnd::Ends,
        };
        body.push(Stmt::Opened(Opening { id, gates, pair, end }));
    }

    /// A `Saved` statement: 3 to 14 steps, each naming only units and tags that are in the box.
    fn saved_values(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let steps = super::saving::picked_steps(id, &mut |n| self.choice(n));
        body.push(Stmt::Saved(super::saving::Saving { id, steps }));
    }

    /// A `Stored` statement: 3 to 10 steps, a fifth of them transactions.
    fn stored_rows(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        body.push(Stmt::Stored(super::storing::picked(id, &mut |n| self.choice(n))));
    }

    /// A `Crewed` statement: tasks changing a `shared Bag` under two or three locks each, one after
    /// another, and `main` reading it after each.
    fn locked_crews(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        body.push(Stmt::Crewed(super::crews::picked(id, &mut |n| self.choice(n))));
    }

    /// A `Pooled` statement: a pool of 1 to 3 links taken by `main` and a crashing task.
    fn keeper_pools(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        body.push(Stmt::Pooled(super::pools::picked(id, &mut |n| self.choice(n))));
    }

    /// A `Messaged` statement: 2 to 8 enums of one shape sent to a task.
    fn enum_messages(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        body.push(Stmt::Messaged(super::messaging::picked(id, &mut |n| self.choice(n))));
    }

    /// A `Sized` statement: one size, a start near its middle or either end, then 3 to 12 steps,
    /// each one's value worked out from the number as it is then, so only a risky program's
    /// steps stop the program, and then one in five.
    fn sized_numbers(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let num = SIZE_NUMS.get(self.choice(SIZE_NUMS.len())).copied().unwrap_or(SizeNum::U8);
        let (least, most) = num.range();
        let low = if num.unsigned() { 3 } else { least + 4 };
        // A start past the largest int can't be written yet, so a `u64` starts below it.
        let start = [7, most - 4, low].get(self.choice(3)).copied().unwrap_or(7).min(i128::from(i64::MAX));
        let mut x = start;
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(10) {
            let breaks = self.risky && self.choice(5) == 0;
            let mut way = SIZED_WAYS.get(self.choice(SIZED_WAYS.len())).copied().unwrap_or(SizedWay::Added);
            if num == SizeNum::I64 && !INT_WRAPPING_METHODS {
                way = match way {
                    SizedWay::WrappingAdd | SizedWay::SaturatingAdd => SizedWay::Added,
                    SizedWay::WrappingSub | SizedWay::SaturatingSub => SizedWay::Taken,
                    SizedWay::WrappingMul | SizedWay::SaturatingMul => SizedWay::Times,
                    other => other,
                };
            }
            let step = sized_choice(num, x, way, breaks, self.choice(4));
            steps.push(step);
            match sized_step(num, x, step.way, step.operand) {
                Ok(_) if matches!(step.way, SizedWay::Summed | SizedWay::Widened | SizedWay::Parsed) => {}
                Ok(after) => x = after,
                Err(_) => break,
            }
        }
        body.push(Stmt::Sized(Sizing { id, num, start, steps }));
    }

    /// An `Arrayed` statement: 3 to 10 steps, each with a place from 0 to 3 and a value from -9 to
    /// 9. Only a risky program indexes past an array's end, which stops it, and then half the time.
    fn arrays(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let mut way = ARRAY_WAYS.get(self.choice(ARRAY_WAYS.len())).copied().unwrap_or(ArrayWay::Set);
            if way == ArrayWay::ForRefSlots && !NARROWED_FIELD_WRITES {
                way = ArrayWay::SlotRead;
            }
            let at = self.choice(4);
            let value = i64::from(self.byte() % 19) - 9;
            let past =
                self.risky && matches!(way, ArrayWay::Looped | ArrayWay::Picked) && self.choice(2) == 0;
            steps.push(ArrayStep { way, at, value, past });
        }
        body.push(Stmt::Arrayed(Arraying { id, steps }));
    }

    /// A `Generated` statement: 3 to 10 steps, each yielding 1 to 5 items, failing at 0 to 6.
    fn failing_generators(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let way = GEN_WAYS.get(self.choice(GEN_WAYS.len())).copied().unwrap_or(GenWay::ForFrom);
            let limit = i64::from(self.byte() % 5) + 1;
            let fail_at = i64::from(self.byte() % 7);
            let other_at = i64::from(self.byte() % 7);
            steps.push(GenStep { way, limit, fail_at, other_at });
        }
        body.push(Stmt::Generated(Generating { id, steps }));
    }

    /// An `Ordered` statement: 3 to 10 steps, each with what its call adds, from 0 to 9.
    fn in_order(&mut self, body: &mut Vec<Stmt>) {
        self.made += 1;
        let id = self.made;
        let base = i64::from(self.byte() % 10);
        let mut steps = Vec::new();
        for _ in 0..3 + self.choice(8) {
            let step =
                ORDER_STEPS.get(self.choice(ORDER_STEPS.len())).copied().unwrap_or(OrderStep::Receiver);
            steps.push((step, i64::from(self.byte() % 10)));
        }
        body.push(Stmt::Ordered(InOrder { id, base, steps }));
    }

    /// A `Looped` step taking an item out of `to`, which holds `kind`.
    fn looping(&mut self, to: usize, kind: Held) -> Looping {
        let tabled = matches!(kind, Held::Table | Held::Entries | Held::Ledger | Held::Rooms);
        let between = match self.choice(3) {
            0 => Between::Changed,
            1 => Between::Grown([2, 6, 20].get(self.choice(3)).copied().unwrap_or(2)),
            // A table's item taken out stops the program on the next turn, so that's rare.
            _ if tabled && self.choice(6) != 0 => Between::Changed,
            _ => Between::Removed,
        };
        Looping {
            to,
            at: self.choice(2),
            n: i64::from(self.byte() % 19) - 9,
            turns: 2 + i64::from(self.byte() % 2),
            between,
            inside: self.choice(3) == 0,
            items: matches!(kind, Held::Holders | Held::Keepers) && self.choice(2) == 0,
        }
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
            1 => FloatHeld::F32Gauge,
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
                    // In a loop, a node holding the list itself would double it every round.
                    let node = self.node_for_list();
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
