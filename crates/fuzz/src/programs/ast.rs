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
}

/// The types a value made anywhere can have. A `shared Point` and a lent `Tally` come from their
/// own statements.
pub(crate) const TYPES: [Ty; 16] = [
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
    /// `copy(name)`: a name's value, copied on purpose, which a loop needs where the name may
    /// change while the copy's held.
    Copy(String),
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
    /// `ref slot = held.items[at]` then `slot += amount`: a place outside the list crashes at the
    /// `ref`.
    RefItem(i64, Amount),
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
}
