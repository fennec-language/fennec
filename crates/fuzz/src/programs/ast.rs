//! The programs the `programs` target writes, as a tree: typed by construction, so every one the
//! generator makes is one the checker takes. The same tree is written out as Fennec and run by
//! the reference evaluator.

/// The types a program's values have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    Int,
    Bool,
    Str,
    Ints,
    Point,
    Shape,
    /// `u8`, `i32` or `u64`.
    Sized(Size),
    /// `shared Point`, reached only through `lock`.
    Shared,
    /// What `tallied` lends a `with` block.
    Tally,
    /// `shared Bag`: a list and a dict, reached only through `lock`.
    Bag,
    /// `Holder`: a struct holding a list.
    Holder,
    /// `dict[str, list[int]]`.
    Lists,
    /// `() -> int`: a function that sums the list it captured.
    Count,
    Float,
    /// `dict[str, int]`.
    Counts,
    /// `Buffer[int]`.
    Buf,
    /// `shared Outer`: a number and a `shared Bag`.
    Outer,
    /// `Node`: an enum whose cases hold `str`, `list[str]` and `list[Node]`, a tree.
    Node,
    /// `list[Node]`.
    Nodes,
    /// A `Node` from `deep`, thousands of levels deep, only measured, never printed whole.
    Deep,
    /// `Named`: a recursive enum with no list of itself, `Pair` holding two more, which is only
    /// ever read through its digest, never printed, compared, shared or sent, so native code can
    /// keep it in blocks of its own.
    Named,
    /// A `Named` from `chain`, thousands of pairs deep, only measured.
    Chain,
}

/// The types a value made anywhere can have. A `shared Point` and a lent `Tally` come from their
/// own statements.
pub(crate) const TYPES: [Ty; 17] = [
    Ty::Int,
    Ty::Bool,
    Ty::Str,
    Ty::Ints,
    Ty::Point,
    Ty::Shape,
    Ty::Holder,
    Ty::Lists,
    Ty::Float,
    Ty::Sized(Size::U8),
    Ty::Sized(Size::I32),
    Ty::Sized(Size::U64),
    Ty::Counts,
    Ty::Buf,
    Ty::Node,
    Ty::Nodes,
    Ty::Named,
];

/// A sized integer type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Size {
    U8,
    I32,
    U64,
}

impl Size {
    /// Its name in Fennec.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Size::U8 => "u8",
            Size::I32 => "i32",
            Size::U64 => "u64",
        }
    }

    /// The least and the most it holds.
    pub(crate) const fn range(self) -> (i128, i128) {
        match self {
            Size::U8 => (0, 255),
            Size::I32 => (-2_147_483_648, 2_147_483_647),
            Size::U64 => (0, 18_446_744_073_709_551_615),
        }
    }
}

/// An arithmetic operator on ints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// A comparison between ints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Compare {
    Less,
    LessEq,
    Equal,
    NotEqual,
    Greater,
}

/// A case of `Shape`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Case {
    Circle,
    Square,
    Empty,
}

#[derive(Clone, Debug)]
pub(crate) enum Expr {
    Int(i64),
    Bool(bool),
    Str(String),
    Name(String),
    Arith(Arith, Box<Expr>, Box<Expr>),
    /// `left % 1009`, keeping a number small where a program isn't meant to fail.
    Small(Box<Expr>),
    Compare(Compare, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// `a if condition else b`.
    Choose(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `left + right` on strings.
    Concat(Box<Expr>, Box<Expr>),
    /// `f"<{int}>"`.
    Format(Box<Expr>),
    /// `text.len()`.
    TextLen(Box<Expr>),
    /// A list of ints written out.
    Ints(Vec<Expr>),
    /// `[item * by + plus for item in list]`, or over `parallel(list)` when the flag says so.
    Mapped(Box<Expr>, Box<Expr>, Box<Expr>, bool),
    /// `sum(item * by for item in parallel(list))`.
    ParallelSum(Box<Expr>, Box<Expr>),
    /// `sum(regrown(item) for item in parallel(list))`: each round makes a holder, reads its list
    /// and passes it to a `ref self` method on the same holder.
    Regrown(Box<Expr>),
    /// `list.len()`.
    Len(Box<Expr>),
    /// `list[at]`, which fails outside the list.
    Index(Box<Expr>, Box<Expr>),
    /// `at_or_zero(list, at)`, which never fails.
    AtOrZero(Box<Expr>, Box<Expr>),
    /// `sum(list)`.
    Sum(Box<Expr>),
    /// `Point(x=..., y=...)`.
    Point(Box<Expr>, Box<Expr>),
    /// `point.x` or `point.y`.
    Field(Box<Expr>, bool),
    /// `point.total()`.
    Total(Box<Expr>),
    /// A shape of a case, with its number.
    Shape(Case, Box<Expr>),
    /// `area(shape)`.
    Area(Box<Expr>),
    /// `bump(ref name, by)`: adds `by` to an int name and gives ten times what it holds then.
    Bump(String, Box<Expr>),
    /// `u8(int)`, `i32(int)` or `u64(int)`, which fails outside what the size holds.
    Convert(Size, Box<Expr>),
    /// `left + - * right` on one size, checked against it.
    SizedArith(Size, Arith, Box<Expr>, Box<Expr>),
    /// `left.wrapping_add(right)` on one size.
    SizedWrap(Size, Box<Expr>, Box<Expr>),
    /// `int(sized)`, which fails for a `u64` past the ints.
    IntOf(Box<Expr>),
    /// `sum(counted(limit))`, through a generator.
    SumCounted(Box<Expr>),
    /// `tally.count`, in the `with` block it's lent to.
    Count(String),
    /// `Holder(items=list, tag=int)`.
    Holder(Box<Expr>, Box<Expr>),
    /// `holder.items`.
    HeldItems(Box<Expr>),
    /// `holder.tag`.
    Tag(Box<Expr>),
    /// A `dict[str, list[int]]` written out.
    Lists(Vec<(&'static str, Expr)>),
    /// `lists.get(key) else list[int]()`.
    ListAt(Box<Expr>, &'static str),
    /// `() => sum(list)`, capturing a name that can't change.
    Summing(String),
    /// `count()`.
    Called(String),
    /// A `dict[str, int]` written out.
    Counts(Vec<(&'static str, Expr)>),
    /// `counts.get(key) else 0`.
    CountOf(Box<Expr>, &'static str),
    /// `key in counts`.
    Has(Box<Expr>, &'static str),
    /// `Buffer[int]()`, or with `capacity=`, then each item pushed.
    Buffer(Option<i64>, Vec<Expr>),
    /// `[item for item in buffer]`.
    BufItems(Box<Expr>),
    /// `buffer.len()`.
    BufLen(Box<Expr>),
    /// `buffer[at]`, which fails outside what's filled.
    BufAt(Box<Expr>, Box<Expr>),
    /// `Node.Leaf(name=text)`.
    Leaf(Box<Expr>),
    /// `Node.Number(value=int)`.
    Number(Box<Expr>),
    /// `Node.Branch(label=text, children=nodes)`.
    Branch(Box<Expr>, Box<Expr>),
    /// `Node.Tagged(tags=texts, inner=nodes)`.
    Tagged(Box<Expr>, Box<Expr>),
    /// A `list[str]` written out.
    Texts(Vec<Expr>),
    /// A `list[Node]` written out.
    NodeList(Vec<Expr>),
    /// `node_or_leaf(nodes, at)`, which never fails.
    NodeAt(Box<Expr>, Box<Expr>),
    /// `digest(node)`: the tree as text, through the program's own recursive function.
    Digest(Box<Expr>),
    /// `digest_all(nodes)`.
    DigestAll(Box<Expr>),
    /// `deep(depth, label)`: a chain of branches `depth` deep, built in a loop.
    Deep(i64, Box<Expr>),
    /// `Named.Leaf(name=text)`.
    NamedLeaf(Box<Expr>),
    /// `Named.Pair(left=named, right=named, label=text, tags=texts)`.
    NamedPair(Box<Expr>, Box<Expr>, Box<Expr>, Box<Expr>),
    /// `chain(depth, label)`: pairs `depth` deep down their left side, built in a loop.
    Chain(i64, Box<Expr>),
    /// `named_digest(named)`, the only way a program reads a whole `Named`.
    NamedDigest(Box<Expr>),
    /// `named_depth(named)`, walking left sides in a loop.
    NamedDepth(Box<Expr>),
    /// `depth_of(node)`, walking first children in a loop.
    DepthOf(Box<Expr>),
    /// A float written out.
    Float(f64),
    /// `left + - * / right` on floats: `/` by zero fails.
    FloatArith(Arith, Box<Expr>, Box<Expr>),
    /// `float(int)`.
    FloatOf(Box<Expr>),
    /// `int(float)`, which fails outside the ints, and for infinity and NaN.
    Truncated(Box<Expr>),
    /// A comparison between floats.
    FloatCompare(Compare, Box<Expr>, Box<Expr>),
}

/// A number written into a lock's body: a literal, or what a task was sent, times something.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Amount {
    Literal(i64),
    /// `number * by`, in a task's body.
    Sent(i64),
}

/// A change to a `Bag`'s list or dict under `lock bag as ref held:`.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    /// `held.items.append(amount)`.
    Append(Amount),
    /// `_ = held.items.pop()`.
    Pop,
    /// `held.items[at] = amount`, or `+=` with `add`: a place outside the list crashes.
    SetItem(i64, Amount, bool),
    /// `held.counts[key] = amount`, or `+=` with `add`: a key that isn't there crashes then.
    SetCount(&'static str, Amount, bool),
    /// `_ = held.counts.remove(key)`.
    Remove(&'static str),
    /// `held.add_twice(amount)`, a `ref self` method appending it twice.
    AddTwice(Amount),
    /// `grow_all(ref held.items, amount)`, through a `ref` parameter.
    GrowItems(Amount),
    /// `ref slot = held.items[at]`, then `slot += amount` and `slot = slot * 2`, reading it: a
    /// place outside the list crashes at the `ref`.
    RefItem(i64, Amount),
    /// `mut kept = held.counts`, `kept[key] = amount`, then `held.counts = kept`: the dict
    /// changed through a copy and stored back, which a failure or a crash must undo too.
    CountsCopied(&'static str, Amount),
    /// `held.items = held.items` and `held.counts = held.counts`, which change nothing.
    SelfAssigned,
    /// `_ = held.counts.pop(key)`, which crashes on a key that isn't there.
    PopCount(&'static str),
}

/// What an arrow function that fails is given to.
#[derive(Clone, Debug)]
pub(crate) enum Arrow {
    /// `tried`, which prints the value or what it failed with.
    Tried,
    /// `tried_twice`, which calls it twice, passing a failure up with `?` to the call's `else`.
    Twice,
    /// `tried_each` with a list, the arrow taking each item.
    Each(Expr),
    /// `ran_fine`, printing whether it gave its value rather than failing.
    RanFine,
    /// `either`, the arrow failing two ways: `capped(n)?`, then `shut_below(other)?`.
    Either(Expr),
}

/// How round `at` of a `Left` loop leaves its `with` blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leave {
    /// `got = present(i, at) else return`.
    ElseReturn,
    /// `got = present(i, at) else fail Refused(code=i)`.
    ElseFail,
    /// `got = present(i, at) else continue`.
    ElseContinue,
    /// `got = present(i, at) else break`.
    ElseBreak,
    /// `got = capped(i, limit=at - 1)?`, a line of its own.
    Question,
    /// `total = total + capped(i, limit=at - 1)? * 2`, inside an expression.
    QuestionInside,
    /// `return` inside an `if`, inside a `match`.
    NestedReturn,
    /// `return` under `if i == at`.
    Return,
    /// `break` under `if i == at`.
    Break,
    /// `continue` under `if i == at`.
    Continue,
    /// Indexing `[1, 2]` out of range under `if i == at`, a crash.
    Crash,
}

impl Leave {
    /// Whether leaving this way is a failure, which runs the cleanups in `lend x else:`, rather
    /// than the code after `lend`.
    pub(crate) const fn fails(self) -> bool {
        matches!(self, Self::ElseFail | Self::Question | Self::QuestionInside)
    }
}

/// What holds a float in `FloatKeys`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FloatHeld {
    /// `Gauge(at=value, n=1)`, a struct's `float` field.
    Gauge,
    /// `Gauge32(at=f32(value))`, a struct's `f32` field.
    Gauge32,
    /// `Level.At(value=value)`, an enum case's `float`, or `Level.Off`.
    Level,
    /// `Level32.At(value=f32(value))`, or `Level32.Off`.
    Level32,
    /// A `Gauge` held as `Marked`, a union with `str`, its set holding `"a"` too.
    Marked,
}

/// The floats `FloatKeys` picks from, each with the key it is: `0.0` and `-0.0` are one, as are
/// two NaNs made differently. The last, `Off`, is only for an enum.
pub(crate) const FLOATS: [(&str, u8); 8] =
    [("0.0", 0), ("-0.0", 0), ("nan()", 1), ("-nan()", 1), ("1.5", 2), ("-1.5", 3), ("2.0", 4), ("Off", 5)];

/// How a union's member is called on.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Via {
    /// After `match store:` with a case for each member, which narrows it as `is` does.
    Match,
    /// After `if store is Member:` for each member, `Lite`'s through `opened`, a function
    /// whose last line is a `with` that returns.
    Is,
    /// `run` straight on the union, with no match, and its `with` source after `is`.
    Direct,
    /// After `if store is Lite: return`, on what's left, `Pg | Mem`: `run` called on it, it
    /// given where a `Pg | Mem` goes, and a `match` telling the two apart. Three members only.
    Rest,
    /// `with store.session()` and `run` straight on the union, with no `is` or `match`: each
    /// runs the member's own.
    Plain,
}

/// The containers `CopiedNested` copies and changes deep inside.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Nested {
    /// `dict[str, Unit]`, a unit's `tags` appended to.
    DictOfUnits,
    /// `Army`, a struct holding a `dict[str, Unit]`.
    ArmyOfUnits,
    /// `dict[str, dict[str, int]]`, an inner number added to.
    DictOfDicts,
    /// `list[dict[str, int]]`.
    ListOfDicts,
    /// `dict[str, list[Unit]]`.
    DictOfUnitLists,
    /// `dict[str, Unit]`, a unit's `hp` added to.
    DictOfUnitsHp,
    /// `dict[str, list[int]]`, a list appended to.
    DictOfIntLists,
    /// `dict[str, Unit]`, a unit replaced by a new one.
    ReplacedInCopy,
}

/// What a local `ref` names: a field or an item of something held in a `mut` name, its index
/// worked out once, at the `ref`.
#[derive(Clone, Debug)]
pub(crate) enum RefPlace {
    /// `list[at]`.
    Item(String, Expr),
    /// `point.x` or `point.y`.
    Field(String, bool),
    /// `holder.items[at]`.
    HeldItem(String, Expr),
    /// `holder.items`, the whole list.
    HeldList(String),
    /// `holder.tag`.
    Tag(String),
    /// `nodes[at]`, a tree.
    NodeItem(String, Expr),
}

/// A change to a `Buffer[int]`. Each fails where a list's would, and `pop` on an empty one.
#[derive(Clone, Debug)]
pub(crate) enum BufOp {
    Push(Expr),
    /// `_ = buffer.pop()`.
    Pop,
    /// `buffer[at] = value`.
    Set(Expr, Expr),
    /// `buffer.insert(at, value)`, which fails past the end.
    Insert(Expr, Expr),
    /// `_ = buffer.remove(at)`.
    Remove(Expr),
    /// `buffer.truncate(count)`.
    Truncate(Expr),
    /// `buffer.extend(other)`, another buffer.
    Extend(String),
}

/// Where an assignment writes.
#[derive(Clone, Debug)]
pub(crate) enum Place {
    Name(String),
    /// `list[at]`.
    Item(String, Expr),
    /// `point.x` or `point.y`.
    Field(String, bool),
}

#[derive(Clone, Debug)]
pub(crate) enum Stmt {
    /// `name = value`, or `mut name = value`.
    Let {
        name: String,
        value: Expr,
        changeable: bool,
    },
    /// `place = value`, or `place += value` when `add` says so.
    Assign {
        place: Place,
        value: Expr,
        add: bool,
    },
    /// `list.append(value)`.
    Append(String, Expr),
    /// `_ = list.pop()`.
    Pop(String),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    /// `for name in range(count):`.
    Range(String, Expr, Vec<Stmt>),
    /// `for name in list:`, over a list nothing in the body changes.
    Each(String, Expr, Vec<Stmt>),
    /// `for ref name in list:`, the body adding to each item.
    EachRef(String, String, Expr),
    /// `for ref name in list:` with `name = name + at_or_zero(list, at)`, reading the list it's
    /// changing: at a fixed place, or where the item itself says when `at` is None.
    EachRefRead(String, String, Option<i64>),
    /// `for ref name in list:` with `name += 1`, then printing `list[at]`: the item changed
    /// through the `ref` has to read back changed by its place, in the same round, which #1202
    /// fixed.
    EachRefThenRead(String, String, i64),
    /// `lock name as held:` printing an `Outer`'s number.
    PrintOuter(String),
    /// A `Table[Point]` made with a point for each of `xs`, one of them taken out when `removed`
    /// says, then `for ref point in table:` adding `by` to each point's `x` and printing the `x` of
    /// the point at `read` by its `Id`, which has to read live, then the table.
    TableRound {
        table: String,
        ids: Vec<String>,
        xs: Vec<Expr>,
        removed: Option<usize>,
        read: usize,
        by: Expr,
    },
    /// `shared name = Outer(inner=bag, n=0)`.
    ShareOuter(String, String),
    /// `bump_outer(name, by, refuse=...)`: adds to its number under its lock, and a refusal
    /// undoes it, printing `refused`.
    OuterBump(String, Expr, Expr),
    /// `summed_until(limit, stop, stdout)`: sums a generator lending from a source until an item
    /// is `stop`, letting it go there. The source prints as it opens and closes, and its cleanup
    /// fails for an even `limit`, which fails the loop: that prints `shut`.
    Summed(Expr, Expr),
    /// An arrow function that fails, `() => capped(n, limit=limit)?`, its `?` passing its own
    /// failure up to what it's given to, as #1305 has it. `capped` fails with `n` when it's over
    /// `limit`.
    Arrowed(Arrow, Expr, i64),
    /// `guarded_shown(code, refuse, crash, noted)`: a `with` on `guarded`, a source whose `lend
    /// x else:` cleanup runs when the block fails or crashes. The cleanup fails for a `code`
    /// that's a multiple of 3, and, in `guarded_noted`, through `capped(code, limit=20)?` for one
    /// over 20: either becomes a note on the block's failure, and the rest of the cleanup is
    /// skipped. `crash` indexes `[1, 2]` by `code`, which crashes unless it's -2 to 1.
    Guarded {
        code: i64,
        refuse: bool,
        crash: bool,
        noted: bool,
    },
    /// `guard_all(items)`: a launched task runs a `with guarded(item)` for each item it's sent,
    /// crashing on 5 and failing on 7, each running the cleanup. Either prints `stopped`.
    GuardAll(Vec<i64>),
    /// `left{id}(stdout)`: a loop of `rounds` rounds, each opening `depth` `ward`s, in nested
    /// `with` blocks or `together` in one, whose cleanups print. Round `at` leaves them all at
    /// once by `how`; an `at` of `rounds` never does. A failure is printed as `refused`.
    Left {
        id: usize,
        depth: usize,
        together: bool,
        rounds: i64,
        at: i64,
        how: Leave,
    },
    /// Keys holding a float or an `f32`, as #1306 has them: a dict with a key for each of
    /// `values`, places in `FLOATS`, a set of them, a lookup of `values[probe]`, `in` with
    /// `FLOATS[asked]`, and `==` and `!=` on `pair`, two places in `values`.
    FloatKeys {
        held: FloatHeld,
        names: (String, String),
        values: Vec<usize>,
        probe: usize,
        asked: usize,
        pair: (usize, usize),
    },
    /// `while counter < limit:`, counting up each round so it ends.
    While(String, i64, Vec<Stmt>),
    /// `if condition: break`.
    Break(Expr),
    /// `if condition: continue`.
    Continue(Expr),
    /// `point.moved(by)`.
    Moved(String, Expr),
    /// `match shape:` adding `by` to whichever number its case holds, through `ref`.
    Grow(String, Expr),
    Print(Vec<(Expr, Ty)>),
    /// `shared name = Point(x=..., y=...)`.
    Share(String, Expr, Expr),
    /// `change(name, by, refuse)`, which under `lock name as ref` adds `by` to `x` and doubles
    /// `y`, then fails if `refuse`, which undoes both. A failure prints `refused`.
    Change(String, Expr, Expr),
    /// `lock name as held:` printing it.
    PrintShared(String),
    /// `shared name = Bag(items=list, counts={...})`.
    ShareBag(String, Expr, Vec<(&'static str, i64)>),
    /// `locked<id>(name, refuse=...)`: a function of its own takes the bag's lock, makes each
    /// change, then fails if asked to, which undoes them all. A failure prints `refused`.
    Locked {
        id: usize,
        name: String,
        ops: Vec<Op>,
        refuse: Expr,
    },
    /// `sent<id>(name, items=list)`: a launched task makes each change for each number it's sent,
    /// under the lock each time. One that crashes undoes that round's changes and prints
    /// `crashed`.
    Sent {
        id: usize,
        name: String,
        ops: Vec<Op>,
        items: Expr,
    },
    /// `tally(name, items=list)`: a launched task takes each item from its inbox and, under the
    /// lock, counts it in `y` and adds it to `x`. A task that overflows crashes, which undoes
    /// that lock's changes and prints `crashed`.
    Tally(String, Expr),
    /// `for name in range(list.len()):` reading `list[name]` first each round, with the body free
    /// to change the list, so a bound worked out once can stop holding.
    Indexed(String, String, Vec<Stmt>),
    /// `counts[key] = value`, or `+=` with `add`, which fails for a key that isn't there.
    SetKey(String, &'static str, Expr, bool),
    /// `_ = counts.remove(key)`.
    RemoveKey(String, &'static str),
    /// A change to a `Buffer[int]`.
    Buffered(String, BufOp),
    /// A change to a `list[Node]`: an append, a pop, or an item set where it's kept in the list.
    NodesAppend(String, Expr),
    NodesPop(String),
    /// `if nodes.len() > 0: nodes[at % nodes.len()] = node`.
    NodesSet(String, Expr, Expr),
    /// `match node:` changing whichever case it is through `ref`: a branch's label grows by `text`
    /// and its children by `child`, a tagged node's tags and inner nodes likewise, a leaf's name
    /// grows and a number counts up.
    Reshape(String, Expr, Expr),
    /// `match named:` changing it through `ref`: a pair's label grows by `text`, its tags gain
    /// the new label, and its left side becomes `left`; a leaf's name grows by `text`.
    Rename(String, Expr, Expr),
    /// `holder.items.append(holder.tag)`, `own = holder.items`, then `holder.grown_by(own)`: a
    /// `ref self` method given a value read from the list it grows, just after that list grew in
    /// place, and both printed, under `if holder.items.len() < 64:`.
    GrownBy(String, String),
    /// `if list.len() < 64:` then `own = list`, `appended_all(ref list, own)` and both printed:
    /// a list lent to a call through `ref` beside a value read from it.
    LentRef(String, String),
    /// `mut rows = [items, [7]]`, then under `if rows[0].len() < 64:`, `own = rows[0]`,
    /// `appended_all(ref rows[0], own)` and the rows and `own` printed: a list's item lent.
    LentItem(String, String, Expr),
    /// `holder.items = holder.items` and `holder.tag = holder.tag`, which change nothing.
    SelfField(String),
    /// `shadowed<id>(stdout, by=value)`, a function whose locals are named `change`,
    /// `bump_outer` and `tally`, like functions that take a `shared` value and may go uncalled.
    Shadowed(usize, Expr),
    /// Collect, then apply: `mut world = [Spot(...), ...]`, then `changes = [plan(item, world)
    /// for item in parallel(xs)]`, a `Change` for each item worked out on the threads from the
    /// world as it is, then a plain loop applying them in order through a local `ref`, and the
    /// world printed.
    PlanApply(String, Vec<(&'static str, Expr)>, Expr, String),
    /// `stdout.print(<how><members>(<member>, text))`: a union of `members` structs, two or
    /// three, each with a `run` method and a `session` `with` source of its own, given the
    /// member numbered `member`, which must be the one whose `session` and `run` are called.
    UnionCall(Via, usize, usize, Expr),
    /// `given = <a container holding containers>`, `mut copy = given`, a change deep inside the
    /// copy by `value`, then both printed: the original mustn't see it, and must still be whole.
    CopiedNested(Nested, String, String, Expr),
    /// `ref name = place`, then the statements that use it, the last of them its last use.
    LocalRef(String, RefPlace, Vec<Stmt>),
    /// `holder.items.append(value)`.
    HolderAppend(String, Expr),
    /// `holder.tag += value`.
    HolderTag(String, Expr),
    /// `lists[key] = list`, storing a list's value.
    PutList(String, &'static str, Expr),
    /// `if key in lists: lists[key].append(value)`.
    ListAppend(String, &'static str, Expr),
    /// `grow_all(ref list, by)`, adding `by` to each item through a `ref` parameter.
    GrowAll(String, Expr),
    /// `for name in counted(limit):`, over a generator.
    EachCounted(String, Expr, Vec<Stmt>),
    /// `with tallied(start, stdout) as name:`, which prints when it opens, when it closes and
    /// when the block crashes.
    With(String, Expr, Vec<Stmt>),
}

/// A whole program: `main`'s body.
#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub(crate) body: Vec<Stmt>,
    /// Whether nothing in it launches a task or runs `parallel`, so its `Named` values can be
    /// native blocks.
    pub(crate) threadless: bool,
}
