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
    /// `u8`.
    Byte,
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
}

/// The types a value made anywhere can have. A `shared Point` and a lent `Tally` come from their
/// own statements.
pub(crate) const TYPES: [Ty; 10] =
    [Ty::Int, Ty::Bool, Ty::Str, Ty::Ints, Ty::Point, Ty::Shape, Ty::Byte, Ty::Holder, Ty::Lists, Ty::Float];

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
    /// `[item * by + plus for item in list]`.
    Mapped(Box<Expr>, Box<Expr>, Box<Expr>),
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
    /// `u8(int)`, which fails outside 0 to 255.
    Byte(Box<Expr>),
    /// `left + right` on `u8`s, checked.
    ByteAdd(Box<Expr>, Box<Expr>),
    /// `left.wrapping_add(right)` on `u8`s.
    ByteWrap(Box<Expr>, Box<Expr>),
    /// `int(byte)`.
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
