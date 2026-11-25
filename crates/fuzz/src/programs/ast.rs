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
    /// `len(text)`.
    TextLen(Box<Expr>),
    /// A list of ints written out.
    Ints(Vec<Expr>),
    /// `[item * by + plus for item in list]`, or over `parallel(list)` when the flag says so.
    Mapped(Box<Expr>, Box<Expr>, Box<Expr>, bool),
    /// `sum(item * by for item in parallel(list))`, which adds in the runtime's fixed tree.
    ParallelSum(Box<Expr>, Box<Expr>),
    /// `sum(float(item) * scale for item in parallel(list))`, which adds in the runtime's fixed
    /// tree, or with `if item % 2 == 0` when the flag says so, which compiled code adds in order,
    /// into eight running totals, as a plain `sum` does. The two give different bits.
    FloatSum(Box<Expr>, Box<Expr>, bool),
    /// `list(range(n))`: a list longer than one leaf of a `parallel` sum's tree, 1,024 positions.
    Upto(usize),
    /// `sum(regrown(item) for item in parallel(list))`: each round makes a holder, reads its list
    /// and passes it to a `ref self` method on the same holder.
    Regrown(Box<Expr>),
    /// `len(list)`.
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
    /// `len(buffer)`.
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
    /// `fallback`, whose failure block gives the value, `why.code * 100`, as its last line.
    Fallback,
    /// `either`, the arrow failing two ways: `capped(n)?`, then `shut_below(other)?`.
    Either(Expr),
}

/// A `Turned` loop: `turned{id}`, of `rounds` turns, its int starting at `start` and changing sign
/// by `how`, stepping down by `step`, or reset to `reset_to` on turn `reset_at`.
#[derive(Clone, Debug)]
pub(crate) struct Turning {
    pub(crate) id: usize,
    pub(crate) how: Turn,
    pub(crate) rounds: i64,
    pub(crate) start: i64,
    pub(crate) step: i64,
    pub(crate) reset_at: i64,
    pub(crate) reset_to: i64,
}

/// A `Streamed` stream: `stream = (f(item) for item in parallel(xs))` over `items`, where `f` is
/// `map`, read by `read`. The runtime runs it as an ordered stream: items worked out on several
/// threads, read in order, a failure cutting it short where it's read.
#[derive(Clone, Debug)]
pub(crate) struct Streaming {
    pub(crate) id: usize,
    pub(crate) items: Vec<i64>,
    pub(crate) map: StreamMap,
    pub(crate) read: StreamRead,
}

/// What a `Streamed` stream works out from each item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamMap {
    /// `item * by + plus`, which can't fail.
    Scaled(i64, i64),
    /// `100 // (item - zero)`, which divides by zero at an item equal to `zero`.
    Divided(i64),
    /// `ys[item]`, of `YS`, which is out of range at an item of 4 or more.
    Indexed,
}

/// The list a `StreamMap::Indexed` stream indexes.
pub(crate) const YS: [i64; 4] = [10, 20, 30, 40];

/// What a name in an `Aliased` function holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    /// `list[int]`.
    Numbers,
    /// `list[Holder]`.
    Holders,
    Holder,
    /// `dict[str, list[int]]`.
    Lists,
    /// `dict[str, Point]`.
    Spots,
    /// `Table[Point]`.
    Table,
    Point,
    Text,
    /// `dict[str, Holder]`: plain structs holding lists, as a dict's values.
    Keepers,
    /// `Ledger{id}`: a struct holding a `Table[Point]` and a count.
    Ledger,
    /// A `Ledger`'s table taken out whole, its items found by the ledger's `Id`s.
    Entries,
    /// `dict[str, int]`.
    Counts,
    /// `Table[Room{id}]`: items each holding a dict of counts and a label.
    Rooms,
}

impl Held {
    /// What a part of it holds: an item, a value at a key, or a field holding a collection.
    pub(crate) const fn part(self) -> Option<Held> {
        match self {
            Held::Holders | Held::Keepers => Some(Held::Holder),
            Held::Holder | Held::Lists => Some(Held::Numbers),
            Held::Spots | Held::Table | Held::Entries => Some(Held::Point),
            Held::Ledger => Some(Held::Entries),
            Held::Rooms => Some(Held::Counts),
            Held::Numbers | Held::Point | Held::Text | Held::Counts => None,
        }
    }

    /// Whether it's a dict whose values a fresh value can be put in at a key.
    pub(crate) const fn keyed(self) -> bool {
        matches!(self, Held::Lists | Held::Spots | Held::Keepers | Held::Counts)
    }

    /// Whether it's a dict, or a table of items holding dicts, that a walk reads.
    pub(crate) const fn walked(self) -> bool {
        matches!(self, Held::Lists | Held::Spots | Held::Keepers | Held::Counts | Held::Rooms)
    }
}

/// The names every `Aliased` function starts with, in order, and what each holds.
pub(crate) const STARTING: [(&str, Held); 10] = [
    ("numbers", Held::Numbers),
    ("holders", Held::Holders),
    ("lists", Held::Lists),
    ("spots", Held::Spots),
    ("table", Held::Table),
    ("text", Held::Text),
    ("keepers", Held::Keepers),
    ("ledger", Held::Ledger),
    ("counts", Held::Counts),
    ("rooms", Held::Rooms),
];

/// Where `STARTING`'s table is, whose `Id`s, `first_id` and `second_id`, its copies share.
pub(crate) const TABLE: usize = 4;

/// Where `STARTING`'s ledger is, whose table's `Id`s are `entry_a` and `entry_b`.
pub(crate) const LEDGER: usize = 7;

/// Where `STARTING`'s rooms are, whose `Id`s are `room_a` and `room_b`.
pub(crate) const ROOMS: usize = 9;

/// The keys of an `Aliased` function's dicts. Each is always there but while a `Removed` step
/// walks what's left, which then puts it back, last. Each list starts with two items and never
/// loses one.
pub(crate) const KEYS: [&str; 2] = ["a", "b"];

/// An `Aliased` function: names starting as copies of others' values or parts, changed through
/// each way of writing in place, then each printed. Each name must hold only what was written
/// to it, never what was written to the one it was copied from, or to one copied from it.
#[derive(Clone, Debug)]
pub(crate) struct Aliasing {
    pub(crate) id: usize,
    /// What the starting values are made from.
    pub(crate) base: i64,
    pub(crate) steps: Vec<Alias>,
}

impl Aliasing {
    /// What each name holds, the starting ones first, then one for each copy in order.
    pub(crate) fn names(&self) -> Vec<Held> {
        let mut held: Vec<Held> = STARTING.iter().map(|(_, held)| *held).collect();
        for step in &self.steps {
            match *step {
                Alias::Copied { from } => held.push(held.get(from).copied().unwrap_or(Held::Numbers)),
                Alias::Part { from, .. } => {
                    held.push(held.get(from).and_then(|from| from.part()).unwrap_or(Held::Numbers));
                }
                Alias::Built { .. } => held.push(Held::Holder),
                Alias::Read { how: Reading::Kept, .. } => held.push(Held::Point),
                Alias::Written { .. }
                | Alias::Tasked { .. }
                | Alias::Walked { .. }
                | Alias::Drained { .. }
                | Alias::Looped(_)
                | Alias::Read { .. } => {}
            }
        }
        held
    }
}

/// The name of an `Aliased` function's `n`th name: a starting one's, or `kept1` on for copies.
pub(crate) fn alias_name(n: usize) -> String {
    STARTING.get(n).map_or_else(|| format!("kept{}", n + 1 - STARTING.len()), |(name, _)| (*name).to_owned())
}

/// A value an `Aliased` function holds, as the reference evaluator keeps it. A dict's entries
/// are in the order their keys went in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    Numbers(Vec<i64>),
    /// Each `Holder`'s items and tag.
    Holders(Vec<(Vec<i64>, i64)>),
    Holder(Vec<i64>, i64),
    Lists(Vec<(String, Vec<i64>)>),
    Spots(Vec<(String, (i64, i64))>),
    /// Each point, the first with `first_id` and the second with `second_id`.
    Table(Vec<(i64, i64)>),
    Point(i64, i64),
    Text(String),
    Keepers(Vec<(String, (Vec<i64>, i64))>),
    /// Its table's points, `entry_a`'s first, and its count.
    Ledger(Vec<(i64, i64)>, i64),
    Entries(Vec<(i64, i64)>),
    Counts(Vec<(String, i64)>),
    /// Each room's counts and label, `room_a`'s first.
    Rooms(Vec<(Vec<(String, i64)>, String)>),
}

/// What `STARTING`'s names hold at first, made from `base`.
pub(crate) fn starting(base: i64) -> Vec<Kept> {
    let keyed = |values: [i64; 2]| -> Vec<(String, i64)> {
        KEYS.iter().zip(values).map(|(key, value)| ((*key).to_owned(), value)).collect()
    };
    vec![
        Kept::Numbers(vec![base, base + 1]),
        Kept::Holders(vec![(vec![base + 2, base + 3], base + 4), (vec![base + 5, base + 6], base + 7)]),
        Kept::Lists(vec![("a".to_owned(), vec![base, base + 8]), ("b".to_owned(), vec![base + 9, base + 1])]),
        Kept::Spots(vec![("a".to_owned(), (base, base + 1)), ("b".to_owned(), (base + 2, base + 3))]),
        Kept::Table(vec![(base, 0), (base + 1, 0)]),
        Kept::Text(format!("t{base}")),
        Kept::Keepers(vec![
            ("a".to_owned(), (vec![base + 1, base + 2], base + 3)),
            ("b".to_owned(), (vec![base + 4, base + 5], base + 6)),
        ]),
        Kept::Ledger(vec![(base + 2, 0), (base + 3, 0)], base),
        Kept::Counts(keyed([base, base + 1])),
        Kept::Rooms(vec![(keyed([base, 1]), "r".to_owned()), (keyed([2, base]), "s".to_owned())]),
    ]
}

/// A step of an `Aliased` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Alias {
    /// `kept{n} = from`, a copy of a whole value.
    Copied { from: usize },
    /// `kept{n} = from[at]`, by place, key or `Id`, or a field holding a collection.
    Part { from: usize, at: usize },
    /// A change to `to`, at place, key or `Id` `at` where it takes one, by `n`.
    Written { to: usize, how: Writing, at: usize, n: i64 },
    /// `[handed{id}_{kind}(from, n=i) for i in parallel(range(3))]`, each task changing its own
    /// copy of what it's handed and giving back a number made from it, printed.
    Tasked { from: usize },
    /// A dict's `values()`, `keys()` and `items()` walked, or each room's dict in a table of
    /// them, printed.
    Walked { from: usize },
    /// `spare{s} = from`, then `kept{n} = Holder(items=spare{s}, tag=n)`: a struct literal taking
    /// a list nothing reads again.
    Built { from: usize, n: i64 },
    /// A `for item in table` loop that only reads, each item borrowed from its slot, over a
    /// `Table`, a ledger's table field or the rooms, done as `how` with `n`. `Reading::Kept`
    /// makes a name: the last item read with an `x` of `n` or more.
    Read { from: usize, how: Reading, n: i64 },
    /// `mut last{s} = from`, its key `at` set to a fresh value made from `n`, then
    /// `print(last{s}.values())` as the dict's last use, so the walk is all that holds what it
    /// gives back once the dict is gone.
    Drained { from: usize, at: usize, n: i64 },
    /// An item taken out, changed and put back in a `while True:` loop whose exit comes after
    /// the put-back, with the item or what holds it changed between turns.
    Looped(Looping),
}

/// A `Looped` step: `mut taken{s} = to[at]`, or that item's `items` when `items`, changed, then
/// `to[at] = taken{s}` in a `while True:` loop that ends after the put-back on its `turns`th
/// turn, `between` done before each turn but the first. Read once before the loop, the put-back
/// comes round again with no read between, and has to put back what was read whatever happened
/// to the item since: the shape the compiled program once got wrong, putting back at a place
/// found before the turn. With `inside`, each turn reads the item first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Looping {
    pub(crate) to: usize,
    pub(crate) at: usize,
    pub(crate) n: i64,
    pub(crate) turns: i64,
    pub(crate) between: Between,
    pub(crate) inside: bool,
    pub(crate) items: bool,
}

/// What a `Looped` step does between its turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Between {
    /// The item changed in place, which the next put-back undoes.
    Changed,
    /// What holds the item grown by this many items, past the room it had.
    Grown(i64),
    /// The item taken out. A table's goes by its `Id`, so the next turn stops the program, as an
    /// `Id` whose item has gone does; a list's by place, another added at its end; a dict's by
    /// key, which the next put-back adds again, last, or a fresh value added at once where the
    /// next turn reads it first or puts back only its `items`.
    Removed,
}

/// The kinds a `Looped` step takes an item out of, and those whose item's `items` it can take.
pub(crate) const fn loops_back(held: Held) -> bool {
    matches!(
        held,
        Held::Holders
            | Held::Keepers
            | Held::Lists
            | Held::Spots
            | Held::Table
            | Held::Entries
            | Held::Ledger
            | Held::Rooms
    )
}

/// How a `Read` step's loop reads a table's items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reading {
    /// Each item's fields summed.
    Summed,
    /// The last item with an `x` of `n` or more kept by another name, printed at the end.
    Kept,
    /// The table replaced, on the first item, by a copy of it held apart, which frees the block
    /// being read; each item read is printed after.
    Replaced,
    /// `break` at the first item whose `x` is over `n`.
    Broken,
    /// `first_over{id}(table, n)`: the first item whose `x` is over `n`, returned from its loop.
    Returned,
    /// `refused_over{id}(table, n)`: the `y`s summed, failing at the first `x` over `n`.
    Failed,
}

/// How an `Aliased` step changes a name, each meaning what it says for the kinds that take it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Writing {
    /// `to.append(n)`, or `to[key].append(n)` / `to[at].items.append(n)` / `to.items.append(n)`.
    Append,
    /// `to[at] = n`, or `to[key][0] = n`.
    Set,
    /// `for ref item in to:`, changing each.
    ForRef,
    /// `to[at].tag = n`, `to[key].x += n`, `to[id].x = n`, `to.tag = n` or `to.x = n`.
    Field,
    /// `mut taken = to[at]`, changed, then `to[at] = taken`.
    PutBack,
    /// A `ref self` method: `grown_by` or `moved`.
    Method,
    /// `poked{id}(ref to, n=n)`, a `ref` parameter.
    Ref,
    /// `to = to + piece`.
    Joined,
    /// `to += piece`.
    Added,
    /// `to = f"{to}piece"`.
    Formatted,
    /// `to = to + piece + to`, a later piece reading what's being grown.
    Chained,
    /// `to += piece + to + "ç"`, three pieces, one of them what's being grown.
    Summed,
    /// `to += piece`, a piece past 23 bytes with letters past ASCII.
    Long,
    /// `_ = to.remove(key)`, a walk of what's left, then the key put back with a new value.
    Removed,
}

/// A `Constants` statement: module constants holding a list, a dict, an empty list of pairs and
/// a struct, made from `base`, read `reads` times on the main thread first, then used and
/// changed in copies by `used{id}(n)`: on the main thread first when `main_first`, in a launched
/// task and in `parallel`'s bodies, in the order `launch_first` says, then on the main thread
/// again. A struct's fields default to the constants and are changed through `ref self`
/// methods. Each use must see the constants as they started, and its changes as made.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Constants {
    pub(crate) id: usize,
    pub(crate) base: i64,
    pub(crate) reads: i64,
    pub(crate) main_first: bool,
    pub(crate) launch_first: bool,
}

/// A `FieldCalls` statement: `box.field += f(...)` on a struct's `str`, `int` and `list[int]`
/// fields, each call printing, failing, or reading the field through another path, so the order
/// the field is read in and the call made shows, and a failure must leave the field as it was.
#[derive(Clone, Debug)]
pub(crate) struct FieldCalls {
    pub(crate) id: usize,
    pub(crate) base: i64,
    pub(crate) steps: Vec<FieldCall>,
}

/// One `+=` of a `FieldCalls`: on `field`, the call made as `call`, with `a + f(b)` when `plus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FieldCall {
    pub(crate) field: Field,
    pub(crate) call: Calling,
    pub(crate) plus: bool,
    pub(crate) n: i64,
}

/// A field of a `FieldCalls` struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Text,
    Count,
    Items,
}

/// What the call on a `FieldCall`'s right side does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Calling {
    /// Prints, then gives a value made from `n`.
    Printed,
    /// `f(n)?` in a function taking the struct by `ref`, failing when `n` is over 5, which is
    /// handled with `except` where it's called.
    Failed,
    /// Reads the field through a copy of the struct, made just before, and prints it.
    Copied,
    /// Reads the field through a list holding a copy of the struct, and prints it.
    Listed,
    /// A closure made just before, which captured the struct, gives the field.
    Closure,
    /// The `+=` is to a `shared` struct's field, under `lock`, with a call that prints.
    Shared,
}

/// A `Yielded` statement: generators yielding a struct holding a text, a list and a dict, one
/// kept in a local and changed across yields, tuples and enum cases holding texts and lists,
/// each value kept and read after more yields, then sent through a task's inbox loop and back.
/// The texts are `widths` long, past the 23 bytes a str holds in place.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Yielding {
    pub(crate) id: usize,
    pub(crate) count: i64,
    pub(crate) widths: [i64; 3],
}

/// A `Woken` statement: a launched task answering each of `count` numbers over a channel, which
/// crashes on `at`, or fails with `Refused` when `failing`, while its launcher waits for the
/// answer. The launcher's wait has to end, and its block with the task's crash or failure.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Waking {
    pub(crate) id: usize,
    pub(crate) count: i64,
    pub(crate) at: i64,
    pub(crate) failing: bool,
    /// How many times it's done, each with a channel of its own made and freed, as many as 70
    /// so threads' caches of the channels they found have to trim.
    pub(crate) rounds: i64,
}

/// A `Swapped` statement: `swapped{id}(given, key, fresh)` keeps what a lookup of `key` in one of
/// `given`'s dicts found, sets the struct whole to another, then stores `fresh` at `key` in the
/// same field, and the value found under `"known"` when `kept`. The store has to go to the dict
/// the struct holds now, never to the caller's, and never into a block let go of: the shape of
/// finding 3 in the review of #1785, which main got wrong.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Swapping {
    pub(crate) id: usize,
    /// What the given struct's values are made from, and the other's.
    pub(crate) base: i64,
    pub(crate) other_base: i64,
    /// The field looked in and stored to.
    pub(crate) field: SwapField,
    /// Which of `SWAP_KEYS` is looked up and stored at.
    pub(crate) key: usize,
    pub(crate) fresh: i64,
    /// Whether the other struct comes from a call, holding `"a"` and `"b"`, rather than written
    /// out where it's named, holding `"c"`.
    pub(crate) called: bool,
    pub(crate) kept: bool,
    /// Whether the caller keeps the struct it gives and prints it after, rather than giving one
    /// it made in the call, which nothing holds once the struct is set to the other.
    pub(crate) owned: bool,
    pub(crate) rounds: i64,
}

/// The field of a `Swapped`'s struct it looks in and stores to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwapField {
    /// `counts`, a `dict[str, int]`.
    Counts,
    /// `lists`, a `dict[str, list[int]]`.
    Lists,
}

/// The keys a `Swapped` looks up: two both structs a call makes hold, and one neither does.
pub(crate) const SWAP_KEYS: [&str; 3] = ["a", "b", "z"];

/// A `Shelved` statement: one pouch in a dict or a list, and each step a task changing that pouch
/// by `how` with a value: past 1, it crashes reading `limits[value]` after the pouch has changed.
/// Where the pouch is held is its `place`. Under a lock, a crash undoes everything done under it,
/// so the shelf is as it was: the shape of finding 1 in the review of #1785, where a crash under a
/// lock put back a freed block. Held by the task alone, a crash lets go of all of it, which main
/// left live in the same review.
#[derive(Clone, Debug)]
pub(crate) struct Shelving {
    pub(crate) id: usize,
    pub(crate) in_list: bool,
    pub(crate) seen: i64,
    pub(crate) place: ShelfPlace,
    /// How many rounds of appending a change makes to the pouch's items, 1 to 6. Past a few, the
    /// items grow out of the list's first block, and a crash with no lock has to let go of the
    /// one they grew into, which main left live in the review of #1785.
    pub(crate) rounds: i64,
    pub(crate) steps: Vec<(i64, Shelve)>,
}

/// Where a `Shelved` statement's pouch is held while a step changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShelfPlace {
    /// In a `shared` shelf, changed under its lock in each step's task, step after step.
    Locked,
    /// In a shelf each step's task makes for itself, with no lock anywhere, which it prints.
    Owned,
    /// In a list or dict each step's task makes for itself, with no struct around it, which it
    /// prints.
    Bare,
}

/// A `Listened` statement: a channel to every listener keeping `size` messages, with one sent
/// before anything listens if `early`, then 1 to `sent` sent while nothing reads. Each of
/// `inboxes` is an inbox made just before those sends and read afterwards in a task of its own,
/// by `receive()` or in a `for` loop, falling behind loudly when `sent` is past `size`. With
/// `dropping`, a task loops over `hits.listen(when_behind=.DropOldest)` from before the sends,
/// saying so over a channel, and reads only once they're all sent, so it hears the last `size`
/// of them however its task is timed.
#[derive(Clone, Debug)]
pub(crate) struct Listening {
    pub(crate) id: usize,
    pub(crate) size: i64,
    pub(crate) early: bool,
    pub(crate) sent: i64,
    pub(crate) dropping: bool,
    /// For each inbox, whether it's read in a `for` loop rather than by `receive()`.
    pub(crate) inboxes: Vec<bool>,
}

/// A `match` with `ref` on a `Named` that starts as `REF_STARTS`'s `start`, adding `REF_TEXTS`'s
/// `text` to the text the case changes, written `way`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RefCasing {
    pub(crate) id: usize,
    pub(crate) way: RefWay,
    pub(crate) start: usize,
    pub(crate) text: usize,
}

/// How many `Named`s a `RefCased` can start from: a leaf, a pair of leaves, and a pair whose left
/// side is a pair, as `source.rs` writes them and `eval.rs` makes them.
pub(crate) const REF_STARTS: usize = 3;

/// What a `RefCased` adds to the text its case changes.
pub(crate) const REF_TEXTS: [&str; 3] = ["!", "hé🦊", ""];

/// The ways a `RefCased` writes its `match`, each one the compiled program put back wrong or
/// couldn't build until the checker put each `ref` case back by its fields' names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefWay {
    /// Three `match`es deep, each over the last one's `ref` name, with fields named and some left
    /// out: the middle one's change wasn't put back.
    Deep,
    /// A whole new value given a `ref` name, the pattern naming its fields out of order and
    /// leaving some out: it wasn't put back.
    NewValue,
    /// A `ref` name the function's parameter has too, that parameter the index of what the
    /// `match` is over: it didn't build.
    Shadowed,
    /// A guard reading the `ref` names: it didn't build.
    Guarded,
    /// The index of what the `match` is over changed in the case, which still goes back where
    /// it matched.
    IndexMoved,
    /// A `for ref` over the case's `ref` list changing its item, then a `?` failing in a `while`
    /// condition: the item's change wasn't put back (#2025).
    ForRefWhile,
    /// The same, the `?` failing in a later `elif`.
    ForRefElif,
    /// The same, the `?` failing after an `and`.
    ForRefAnd,
}

/// Every `RefWay`.
pub(crate) const REF_WAYS: [RefWay; 8] = [
    RefWay::Deep,
    RefWay::NewValue,
    RefWay::Shadowed,
    RefWay::Guarded,
    RefWay::IndexMoved,
    RefWay::ForRefWhile,
    RefWay::ForRefElif,
    RefWay::ForRefAnd,
];

impl RefWay {
    /// Whether it holds what it matches in a list, which it then does as a `Held`, not a `Named`.
    pub(crate) fn holds_in_a_list(self) -> bool {
        matches!(self, RefWay::Shadowed | RefWay::IndexMoved)
    }
}

/// A union of `Crate{id}`, holding a list, and `Heap{id}`, holding an int, each with a `ref self`
/// method `grow` that changes it and gives back its size, grown through a name narrowed to one
/// member, written `way`. It starts as a `Crate{id}` holding `[n]` when `crate_first`, else as a
/// `Heap{id}` holding `n`; where it's given the other member, that one holds `n + 1`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Narrowing {
    pub(crate) id: usize,
    pub(crate) way: NarrowWay,
    pub(crate) crate_first: bool,
    pub(crate) n: i64,
}

/// The ways a `Narrowed` statement narrows its union and changes it. A `ref self` method called on
/// a union narrowed to one member was given the whole union (#2157), and a narrowing outlived a
/// `ref` call that gave the name the other member (#2163).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NarrowWay {
    /// Narrowed by what it's given: grown twice, given the other member, and grown again.
    Given,
    /// Narrowed by `if shape is ...:` for each member, grown and read inside.
    Is,
    /// Given by `ref` to a function where it's grown, narrowed to what's left after
    /// `if shape is Heap{id}: return shape.grow()`.
    Left,
    /// Not narrowed: the union's own `grow`, which runs its member's.
    Whole,
    /// Grown in the `match` case naming its member.
    Matched,
    /// Each item of a list of both, grown in a `for ref` loop, and grown again where `is` narrows
    /// it to a `Crate{id}`.
    ForRef,
    /// Grown as a struct's field, which `is` doesn't narrow.
    Field,
    /// Narrowed, by `is` and by what it's given, and given by `ref` to a function taking the member.
    RefMember,
    /// Narrowed by `is` and given by `ref` to a function taking the union, which grows it and gives
    /// it the other member, then grown after the `if`.
    RefUnion,
    /// Narrowed by what it's given, given by `ref` to the same function, and grown right after,
    /// where the narrowing has to have ended (#2163).
    RefUnionGrown,
}

/// Every `NarrowWay`.
pub(crate) const NARROW_WAYS: [NarrowWay; 10] = [
    NarrowWay::Given,
    NarrowWay::Is,
    NarrowWay::Left,
    NarrowWay::Whole,
    NarrowWay::Matched,
    NarrowWay::ForRef,
    NarrowWay::Field,
    NarrowWay::RefMember,
    NarrowWay::RefUnion,
    NarrowWay::RefUnionGrown,
];

/// A name that another of the same name and type could capture, written `way`, with `n` the value
/// it adds or lends.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Capturing {
    pub(crate) id: usize,
    pub(crate) way: CaptureWay,
    pub(crate) n: i64,
}

/// The ways a `Captured` writes a name another of its name and type could capture, each one the
/// checker renamed by text, not by what the name means where it's written (#1997 and the
/// checker's name-capture sweep).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureWay {
    /// `with opened() as added:` holding a nested function whose parameter is `added` too.
    WithDef,
    /// `lock bag as ref held:` holding a loop whose name is `held` too.
    LockLoop,
    /// `lock bag as ref held:` holding a case whose name is `held` too.
    LockCase,
    /// `lock bag as ref held:` holding a nested function whose parameter is `held` too.
    LockDef,
    /// `lock bag as ref held:` holding an arrow whose parameter is `held` too.
    LockArrow,
    /// A `with` source calling a function whose name its caller has as a parameter, of the same
    /// type.
    SourceCall,
}

/// Every `CaptureWay`.
pub(crate) const CAPTURE_WAYS: [CaptureWay; 6] = [
    CaptureWay::WithDef,
    CaptureWay::LockLoop,
    CaptureWay::LockCase,
    CaptureWay::LockDef,
    CaptureWay::LockArrow,
    CaptureWay::SourceCall,
];

/// How a `Shelved` step changes the pouch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shelve {
    /// `held.fill(value)`, a `ref self` method of the shelf calling the pouch's `ref self` `add`.
    /// With no shelf, the same as `Direct`.
    Fill,
    /// `held.by_hand(value)`: the pouch copied out, changed and stored back. With no shelf, done
    /// in the task's own function.
    ByHand,
    /// `held.pouches[...].add(value)`, the pouch's `ref self` method called straight on the part
    /// of what the shelf holds, or on the task's own list or dict.
    Direct,
    /// `added{id}(ref held.pouches, value)`, a function handed the list or dict itself by `ref`,
    /// which calls `add` on its part.
    Handed,
    /// `by_hand{id}(ref held.pouches, value)`: the same function changing the pouch by hand.
    HandedByHand,
}

impl Shelve {
    /// Whether it changes the pouch by hand, which appends one text, rather than with `add`,
    /// which appends two.
    pub(crate) fn by_hand(self) -> bool {
        matches!(self, Shelve::ByHand | Shelve::HandedByHand)
    }
}

/// How a `Streamed` stream is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamRead {
    /// A `for` loop printing each, breaking after the `n`th when it's given.
    Loop(Option<i64>),
    /// `sum(y for y in stream)`.
    Sum,
    /// `[y + 1 for y in stream]`.
    Listed,
}

/// An `Ordered` statement: `ordered{id}(stdout)`, a `Tally{id}` and a `Pair{id}` holding a copy of
/// it, then a line for each step, in which a call made later changes what was read earlier.
/// Evaluation is left to right, as `docs/decisions.md` says, so each line shows what was read
/// before the change: a method's receiver and a field's receiver before an argument, an index's
/// base before the index, and the left of an operator, a list, a text, a call's arguments, a
/// field or index write and `+=` before what's to their right.
#[derive(Clone, Debug)]
pub(crate) struct InOrder {
    pub(crate) id: usize,
    /// What the tally's items, count and dict start from.
    pub(crate) base: i64,
    /// Each step, with what its `grow` or `key` adds, from 0 to 9.
    pub(crate) steps: Vec<(OrderStep, i64)>,
}

/// A line of an `Ordered` statement, each reading `tally`, and `pair` for `FieldReceiver`, before
/// a call that changes it: `grow(n)`, which appends `count + n` and gives back the count it adds
/// 1 to, or `key(n)`, which adds `n` to `names["a"]` and gives back `"a"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrderStep {
    /// `tally.show(tally.grow(n))`: a method taking its value as a copy.
    Receiver,
    /// `pair.shown(pair.left.grow(n))`: the same through a field.
    FieldReceiver,
    /// `tally.items.count(tally.grow(n))`: a list's method that only reads.
    Counted,
    /// `tally.names.get(tally.key(n))`: a dict's.
    Got,
    /// `tally.items[tally.grow(n) % 4]`: an index's base.
    Indexed,
    /// `tally.names[tally.key(n)]`: a dict's.
    Keyed,
    /// `three{id}(tally.count, second=tally.grow(n), third=tally.count)`.
    Arguments,
    /// `tally.count + tally.grow(n)`.
    PlusBefore,
    /// `tally.grow(n) + tally.count`.
    PlusAfter,
    /// `[tally.count, tally.grow(n), tally.count]`.
    Listed,
    /// `f"{tally.count} {tally.grow(n)} {tally.count}"`.
    Text,
    /// `tally.count >= tally.grow(n)`, false read left to right.
    AtLeast,
    /// `tally.count = tally.grow(n) + n`.
    FieldWrite,
    /// `tally.items[tally.grow(n) % 4] = tally.grow(n)`: the place before what's put there.
    IndexWrite,
    /// `tally.count += tally.grow(n)`: the count before the call.
    Added,
}

/// Every `OrderStep`, for the generator to pick from.
pub(crate) const ORDER_STEPS: [OrderStep; 15] = [
    OrderStep::Receiver,
    OrderStep::FieldReceiver,
    OrderStep::Counted,
    OrderStep::Got,
    OrderStep::Indexed,
    OrderStep::Keyed,
    OrderStep::Arguments,
    OrderStep::PlusBefore,
    OrderStep::PlusAfter,
    OrderStep::Listed,
    OrderStep::Text,
    OrderStep::AtLeast,
    OrderStep::FieldWrite,
    OrderStep::IndexWrite,
    OrderStep::Added,
];

/// A `Filed` statement: `filed{id}(fs, stdout)`, a folder `d{id}` of its own holding two files,
/// `f.txt` and `g.txt`, cleared at the start, then written a step at a time, each step followed by
/// what the file holds. A write through `with fs.create(...)` that fails, passes a failure up with
/// `?` or crashes partway leaves the file as it was, byte for byte, and one that ends well
/// replaces it whole.
#[derive(Clone, Debug)]
pub(crate) struct Filing {
    pub(crate) id: usize,
    pub(crate) steps: Vec<FileStep>,
}

/// A step of a `Filed` statement: how it writes, to `g.txt` rather than `f.txt` when `second`
/// says, the pieces it writes, from `FILE_PIECES`, and the piece it stops at, which lets it finish
/// when it's past the last.
#[derive(Clone, Debug)]
pub(crate) struct FileStep {
    pub(crate) way: FileWay,
    pub(crate) second: bool,
    pub(crate) pieces: Vec<usize>,
    pub(crate) stop_at: usize,
}

/// How a `Filed` step writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileWay {
    /// `saved{id}`: `with fs.create(path)?`, failing with `Halt{id}` before the piece it stops at.
    Saved,
    /// `questioned{id}`: `with fs.create(path, durable=False)?`, syncing each piece, then passing a
    /// failure up with `?` after the piece it stops at.
    Questioned,
    /// `tasked{id}`: the write in a launched task, which crashes after the piece it stops at. In
    /// programs with threads.
    Tasked,
    /// `both{id}`: `f.txt` and `g.txt` in one `with`, failing between the two writes of the piece
    /// it stops at, so neither changes.
    Both,
    /// `fs.write_text(path, text)`.
    Plain,
    /// `fs.read_only().write_text(path, text)`, refused.
    ReadOnly,
    /// `fs.only("d{id}").write_text("elsewhere.txt", text)`, refused as outside it.
    Outside,
    /// `fs.remove(path)`, which says when nothing's there.
    Removed,
}

/// Every `FileWay`, for the generator to pick from.
pub(crate) const FILE_WAYS: [FileWay; 8] = [
    FileWay::Saved,
    FileWay::Questioned,
    FileWay::Tasked,
    FileWay::Both,
    FileWay::Plain,
    FileWay::ReadOnly,
    FileWay::Outside,
    FileWay::Removed,
];

/// What a `Filed` step writes, a piece at a time: lines, text with no line's end, nothing, a
/// letter past ASCII and an emoji, and quotes.
pub(crate) const FILE_PIECES: [&str; 6] = ["one\n", "tw\u{f8} \u{1f98a}\n", "", "three", "\n", "a \"b\" c\n"];

/// An `Owned` statement: `owned{id}(stdout)`, values of types with their own `def []`, each
/// changed through it a step at a time, then printed with how often each `def []=` ran. Through the
/// plain form beside a `def []=`, a change reads the part, changes the copy and writes it back
/// once, nested too, and still once when what changed it fails partway. Through the `ref` form a
/// position is a place, which `=`, a `ref` argument and `for ref` reach, its empty positions
/// skipped. `docs/decisions.md` sets these out, and #2192 wrote back every built-in method.
#[derive(Clone, Debug)]
pub(crate) struct OwnIndexing {
    pub(crate) id: usize,
    /// What the parts' `x`, the rows and the slots start from.
    pub(crate) base: i64,
    /// Each step, with the position it's at, from 0 to 2, and what it adds, from 0 to 9.
    pub(crate) steps: Vec<(OwnStep, usize, i64)>,
}

/// A line of an `Owned` statement. A `Shelf{id}` holds two `Part{id}`s in a list, a `Keyed{id}`
/// one in a dict, a `Room{id}` a shelf, and a `Rows{id}` two lists, each with the plain form of
/// `def []` and a `def []=` that counts its writes. `Slots{id}` has the `ref` form, `len` and
/// `is_live`, its middle position empty. The shelf's and the rows' position is the step's, from 0
/// to 1, and the slots' is from 0 to 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnStep {
    /// `shelf[at].x += n`.
    Plus,
    /// `shelf[at].x = n`.
    Set,
    /// `shelf[at].moved(n)`: a `ref self` method changing it twice, written back once.
    Moved,
    /// `shelf[at].marks.append(n)`: a built-in method on a part's field.
    Appended,
    /// `shelf[at].shift(n) except why:`: a method that fails past 5, after changing the part, which
    /// is written back as far as it got.
    Shifted,
    /// `passed{id}(ref shelf, at=at, by=n) except why:`: the same failure passed up with `?`, so the
    /// change after it is skipped.
    Passed,
    /// `bumped{id}(ref shelf[at].x, by=n)`: a `ref` argument through the plain form.
    RefField,
    /// `shelf[at].x += shelf[other].x`: a read through `def []` beside a write through it.
    PlusOther,
    /// `keyed["k"].x += n`, over a dict.
    Keyed,
    /// `keyed["k"].moved(n)`.
    KeyedMoved,
    /// `room["back"][at].x += n`: two own indexes, each written back once.
    Nested,
    /// `room["back"][at].moved(n)`.
    NestedMoved,
    /// `bumped{id}(ref room["back"][at].x, by=n)`.
    NestedRef,
    /// `rows[at].count(n)` and `rows[at].get(n)`: methods that only read, so nothing's written back.
    Counted,
    /// `rows[at].sort()`.
    Sorted,
    /// `rows[at].sort(key=item => 0 - item)`.
    SortedKey,
    /// `rows[at].remove_where(item => item > n)`.
    Removed,
    /// `slots[at] = n`, through the `ref` form.
    SlotSet,
    /// `slots[at] += n`.
    SlotPlus,
    /// `bumped{id}(ref slots[at], by=n)`.
    SlotRef,
    /// `for ref slot in slots: slot = slot * 2 + n`, the empty position skipped.
    ForRef,
    /// `for ref slot in parallel(slots): slot += n`, in programs with threads.
    ParallelRef,
}

/// Every `OwnStep`, for the generator to pick from.
pub(crate) const OWN_STEPS: [OwnStep; 22] = [
    OwnStep::Plus,
    OwnStep::Set,
    OwnStep::Moved,
    OwnStep::Appended,
    OwnStep::Shifted,
    OwnStep::Passed,
    OwnStep::RefField,
    OwnStep::PlusOther,
    OwnStep::Keyed,
    OwnStep::KeyedMoved,
    OwnStep::Nested,
    OwnStep::NestedMoved,
    OwnStep::NestedRef,
    OwnStep::Counted,
    OwnStep::Sorted,
    OwnStep::SortedKey,
    OwnStep::Removed,
    OwnStep::SlotSet,
    OwnStep::SlotPlus,
    OwnStep::SlotRef,
    OwnStep::ForRef,
    OwnStep::ParallelRef,
];

/// A `Generic` statement: `generic{id}(stdout)`, a generic `Holder{id}[T]` and generic functions
/// given one type, a holder, a second one, a holder of the first, a list of both and a dict holding
/// the second, then a step at a time, each a generic call. Generic code can't look inside a `T`, so
/// each value's text is all the evaluator needs. Copies of `T` made inside generic code must change
/// apart, and a `ref` into a `T` must change the one it names.
#[derive(Clone, Debug)]
pub(crate) struct Genericking {
    pub(crate) id: usize,
    pub(crate) ty: GenericTy,
    /// Each step, with the value it uses, from 0 to 2 in `GenericTy::values`, and a count, from 0
    /// to 2.
    pub(crate) steps: Vec<(GenericStep, usize, usize)>,
}

/// The type a `Generic` statement's `T` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenericTy {
    Int,
    Str,
    Ints,
    /// `Point{id}`: a struct holding a list.
    Point,
}

impl GenericTy {
    /// The type as written, for statement `id`.
    pub(crate) fn written(self, id: usize) -> String {
        match self {
            GenericTy::Int => "int".to_owned(),
            GenericTy::Str => "str".to_owned(),
            GenericTy::Ints => "list[int]".to_owned(),
            GenericTy::Point => format!("Point{id}"),
        }
    }

    /// Three values of the type for statement `id`, each as written and as it prints inside a
    /// struct.
    pub(crate) fn values(self, id: usize) -> [(String, String); 3] {
        let same = |text: &str| (text.to_owned(), text.to_owned());
        match self {
            GenericTy::Int => [same("0"), same("7"), same("-3")],
            GenericTy::Str => [same("\"a\""), same("\"\""), same("\"\u{e9}\u{1f98a}\"")],
            GenericTy::Ints => [same("[1, 2]"), ("list[int]()".to_owned(), "[]".to_owned()), same("[5]")],
            GenericTy::Point => [
                same(&format!("Point{id}(x=1, tags=[\"a\"])")),
                (format!("Point{id}(x=2, tags=list[str]())"), format!("Point{id}(x=2, tags=[])")),
                same(&format!("Point{id}(x=-4, tags=[\"b\", \"c\"])")),
            ],
        }
    }
}

/// Every `GenericTy`, for the generator to pick from.
pub(crate) const GENERIC_TYS: [GenericTy; 4] =
    [GenericTy::Int, GenericTy::Str, GenericTy::Ints, GenericTy::Point];

/// A step of a `Generic` statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenericStep {
    /// `holder.put(v)`: a `ref self` method keeping the old item and taking the new.
    Put,
    /// `grown(ref holder.items, value=v, times=count)`: a `ref list[T]` parameter grown.
    Grown,
    /// `swapped(ref holder.item, ref other.item)`: two `ref T` parameters in two holders.
    Swapped,
    /// `swapped(ref holder.items, ref other.items)`.
    SwappedItems,
    /// `changed_copy(holder, value=v)`, printed: a copy made and changed inside generic code,
    /// leaving the holder as it was.
    Copied,
    /// `doubled(holder.item)`, printed: a list of two copies.
    Doubled,
    /// `holder.oldest()`, printed.
    Oldest,
    /// `outer.item.put(v)`: a holder held in a holder, changed through its field.
    Nested,
    /// `for ref each in pair: each.put(v)`.
    ForRef,
    /// `holder.items.pop() else holder.item`, printed.
    Popped,
    /// `table["k"].put(v)`: a holder in a dict, changed where it is.
    DictPut,
}

/// Every `GenericStep`, for the generator to pick from.
pub(crate) const GENERIC_STEPS: [GenericStep; 11] = [
    GenericStep::Put,
    GenericStep::Grown,
    GenericStep::Swapped,
    GenericStep::SwappedItems,
    GenericStep::Copied,
    GenericStep::Doubled,
    GenericStep::Oldest,
    GenericStep::Nested,
    GenericStep::ForRef,
    GenericStep::Popped,
    GenericStep::DictPut,
];

/// A `Stuck` statement: `stuck{id}(stdout)`, a channel of room for one, then a wait on it that
/// nothing will end. A wait given a time limit fails with `Timeout` once it's up, and the program
/// goes on. One without, `main` receiving or sending, at a `with launch` block's end for a task
/// receiving, or in a `parallel` call's rounds each receiving, is a deadlock: the program stops,
/// with exit code 1, after what it printed, as `docs/language.md` says. One that hung instead
/// would be stopped by the fuzzer as stuck.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sticking {
    pub(crate) id: usize,
    pub(crate) way: StuckWay,
    /// The time limit in milliseconds, from 1 to 20, or how many rounds, from 1 to 4.
    pub(crate) n: i64,
}

/// How a `Stuck` statement waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StuckWay {
    /// `jobs.receive_within(...)` on the empty channel, which times out.
    TimedReceive,
    /// `jobs.send_within(...)` on the full channel, which times out.
    TimedSend,
    /// `jobs.receive()` on the empty channel: a deadlock.
    Received,
    /// A second `jobs.send(...)` on the full channel: a deadlock.
    SentFull,
    /// The end of a `with launch` block whose task receives from the channel: a deadlock. In
    /// programs with threads.
    BlockEnd,
    /// A `parallel` call whose rounds each receive from the channel: a deadlock. In programs with
    /// threads.
    Rounds,
}

/// Every `StuckWay`, for the generator to pick from.
pub(crate) const STUCK_WAYS: [StuckWay; 6] = [
    StuckWay::TimedReceive,
    StuckWay::TimedSend,
    StuckWay::Received,
    StuckWay::SentFull,
    StuckWay::BlockEnd,
    StuckWay::Rounds,
];

/// An `Interfaced` statement: `interfaced{id}(stdout)`, an interface `Grower{id}` with a `ref
/// self` method, a plain one, one that fails partway and a generator, which `Crate{id}`, holding
/// a list, and `Heap{id}`, holding an int, both have, and generic functions bounded by it, then a
/// step at a time on a list of each, and one of each. Each generic call must run the method of
/// the type it's given, change the place a `ref` names, and leave a copy apart.
#[derive(Clone, Debug)]
pub(crate) struct Interfacing {
    pub(crate) id: usize,
    /// Each step, on the crates rather than the heaps when `crate_first` says, with what it adds,
    /// from 0 to 9.
    pub(crate) steps: Vec<(IfaceStep, bool, i64)>,
}

/// A step of an `Interfaced` statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IfaceStep {
    /// `grown_all(ref items, by=n)`, printed: `for ref` over a list of `G`, growing each.
    GrownAll,
    /// `bumped(ref item, by=n)`: two grows through a `ref G`.
    Bumped,
    /// `copied(item, by=n)`, printed: a copy grown inside generic code, the item left as it was.
    Copied,
    /// `summed(item)`, printed: the sum of an interface method that's a generator.
    Summed,
    /// `tried_twice(ref item, by=n)`, printed or its failure: a failing method twice, through `?`.
    Tried,
    /// `for ref each in items: bumped(ref each, by=n)`: a `for ref` item given on by `ref`.
    EachBumped,
    /// `sized(items)`, printed: each one's size, summed.
    Sized,
}

/// Every `IfaceStep`, for the generator to pick from.
pub(crate) const IFACE_STEPS: [IfaceStep; 7] = [
    IfaceStep::GrownAll,
    IfaceStep::Bumped,
    IfaceStep::Copied,
    IfaceStep::Summed,
    IfaceStep::Tried,
    IfaceStep::EachBumped,
    IfaceStep::Sized,
];

/// A `LockedTogether` statement: `locked_together{id}(stdout)`, three `shared` accounts made in
/// order, then a step at a time on two of them taken in one `lock`, each followed by what all
/// three hold. A `fail` or a crash inside `lock ... as ref` puts back every value it changed, of
/// both, as `docs/language.md` says; nothing else does.
#[derive(Clone, Debug)]
pub(crate) struct LockingTogether {
    pub(crate) id: usize,
    /// What the accounts start from.
    pub(crate) base: i64,
    pub(crate) steps: Vec<LockStep>,
}

/// A step of a `LockedTogether` statement on two accounts, `pair` naming them by the order they
/// were made, from 0 to 2, the first before the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LockStep {
    pub(crate) way: LockWay,
    pub(crate) pair: (usize, usize),
    /// What it moves or adds, from 0 to 9.
    pub(crate) amount: i64,
    /// Whether it takes from the second and gives to the first.
    pub(crate) backward: bool,
    /// Whether the `lock` names the second before the first, against the order they were made.
    pub(crate) against: bool,
}

/// How a `LockedTogether` step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockWay {
    /// `moved`: takes from one and gives to the other, both changed, then fails when the one taken
    /// from is below 0, which puts both back.
    Moved,
    /// `crashed`, in a launched task: changes both, then crashes, which puts both back. In
    /// programs with threads.
    Crashed,
    /// `summed`: both read under one `lock`, given back from inside it.
    Summed,
    /// `half_ref`: one changed through `ref` from what the other holds, read.
    HalfRef,
}

/// Every `LockWay`, for the generator to pick from.
pub(crate) const LOCK_WAYS: [LockWay; 4] =
    [LockWay::Moved, LockWay::Crashed, LockWay::Summed, LockWay::HalfRef];

/// A `Ruled` statement: `ruled{id}(stdout)`, a `Gauge{id}` whose four fields have `where` rules,
/// one alone, two more in a list and one in a dict, then a step at a time, then what each holds.
/// A rule runs on every write through its struct, on `copy` and on making a value. A field lent
/// alone by `ref`, to a call or a `for ref` loop's turn, is checked when it comes back, so a value
/// put right before then is fine (#2270). A broken rule stops the program, and one broken making a
/// value from what's worked out as it runs is refused.
#[derive(Clone, Debug)]
pub(crate) struct Ruling {
    pub(crate) id: usize,
    pub(crate) steps: Vec<RuleStep>,
}

/// A step of a `Ruled` statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RuleStep {
    pub(crate) way: RuleWay,
    /// What it adds, takes away or sets, from 0 to 9, and which of two gauges in a list, by its
    /// evenness.
    pub(crate) value: i64,
    /// Whether it breaks its rule, which stops the program, or for `Made`, is refused.
    pub(crate) breaks: bool,
}

/// How a `Ruled` step goes. `level` must stay at least 0, `tags` at most 3 long, `name` not empty
/// and `counts` summing to below 20. What a step takes from `level` is worked out from what it
/// holds, so the step breaks the rule only when it's meant to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuleWay {
    /// `gauge.level = picked(value)`, below 0 when it breaks.
    Set,
    /// `gauge.level += value`.
    Raised,
    /// `gauge.level -= amount`.
    Taken,
    /// `lowered(ref gauge.level, by=amount)`: the field lent alone, checked as the call ends.
    Lowered,
    /// `dipped(ref gauge.level, by=...)`: below 0 and back before the call ends, which is fine.
    Dipped,
    /// `gauge.drained(amount)`: a `ref self` method.
    Drained,
    /// `shrunk(ref gauge, by=amount)`: the whole struct lent, checked at the write.
    Shrunk,
    /// `gauges[at].level -= amount`.
    InList,
    /// `for ref each in gauges`, each one's `level` added to, or taken past 0 when it breaks.
    ForRef,
    /// `for ref each in gauge.counts`, each item past the sum and back within the turn, or left
    /// past it when it breaks, which stops the program as that turn ends.
    Counted,
    /// `gauge.tags.append(...)` while there's room, or four more when it breaks.
    Tagged,
    /// `tagged(ref gauge.tags, ...)` while there's room, or `flooded(ref gauge.tags)` when it
    /// breaks, which prints before the call ends and the rule runs.
    TaggedByRef,
    /// `gauge.name = ...`, emptied when it breaks.
    Named,
    /// `copy(gauge, level=picked(value))`, printed, below 0 when it breaks.
    Copied,
    /// `made(value)`: `Gauge(level=picked(value))`, printed, or refused below 0.
    Made,
    /// `by_name["k"].level -= amount`.
    InDict,
}

/// Every `RuleWay`, for the generator to pick from.
pub(crate) const RULE_WAYS: [RuleWay; 16] = [
    RuleWay::Set,
    RuleWay::Raised,
    RuleWay::Taken,
    RuleWay::Lowered,
    RuleWay::Dipped,
    RuleWay::Drained,
    RuleWay::Shrunk,
    RuleWay::InList,
    RuleWay::ForRef,
    RuleWay::Counted,
    RuleWay::Tagged,
    RuleWay::TaggedByRef,
    RuleWay::Named,
    RuleWay::Copied,
    RuleWay::Made,
    RuleWay::InDict,
];

/// A `Popped` statement: `popped{id}(stdout)`, a list of optionals of one heap type, held in a
/// local or a struct's field, items put in and taken out a step at a time: `pop()`, `pop(at)` at
/// either end and the middle, `remove`, `discard`, and through a `ref` parameter, each kept in a
/// name, used inline with `else` or dropped. A list holds such an optional as an enum, where a
/// name holds it as itself, so taking one out must turn the one into the other (#2282).
#[derive(Clone, Debug)]
pub(crate) struct Popping {
    pub(crate) id: usize,
    pub(crate) ty: PopTy,
    /// Whether the list is a struct's field, rather than a local.
    pub(crate) in_field: bool,
    /// Each step, with the value it uses, from 0 to 2 in `PopTy::values`.
    pub(crate) steps: Vec<(PopWay, usize)>,
}

/// What a `Popped` statement's list holds optionals of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PopTy {
    /// A text short enough to be held in a word or two.
    Short,
    /// A text long enough to be held in a block of its own.
    Long,
    /// `list[int]`, an empty one among them.
    Ints,
    /// `Pt{id}`, a struct of one int.
    Point,
    /// `Bag{id}`, a struct holding a text and a list.
    Bag,
}

impl PopTy {
    /// The type as written, for statement `id`.
    pub(crate) fn written(self, id: usize) -> String {
        match self {
            PopTy::Short | PopTy::Long => "str".to_owned(),
            PopTy::Ints => "list[int]".to_owned(),
            PopTy::Point => format!("Pt{id}"),
            PopTy::Bag => format!("Bag{id}"),
        }
    }

    /// Three values of the type for statement `id`, then what stands in for a missing one: each
    /// as written, as printed alone, and as printed inside a list or a struct.
    pub(crate) fn values(self, id: usize) -> [(String, String, String); 4] {
        let text = |text: &str| (format!("\"{text}\""), text.to_owned(), format!("\"{text}\""));
        let same = |text: &str| (text.to_owned(), text.to_owned(), text.to_owned());
        let long = |at: usize| format!("a word long enough to be held in a block of its own, {at}");
        match self {
            PopTy::Short => [text("kit"), text("fox"), text("\u{e9}\u{1f98a}"), text("none")],
            PopTy::Long => [text(&long(1)), text(&long(2)), text(&long(3)), text("none")],
            PopTy::Ints => [
                same("[1, 2]"),
                ("list[int]()".to_owned(), "[]".to_owned(), "[]".to_owned()),
                same("[3]"),
                same("[9]"),
            ],
            PopTy::Point => [
                same(&format!("Pt{id}(x=5)")),
                same(&format!("Pt{id}(x=-2)")),
                same(&format!("Pt{id}(x=7)")),
                same(&format!("Pt{id}(x=0)")),
            ],
            PopTy::Bag => {
                let bag = |written: String, shown: String| (written, shown.clone(), shown);
                [
                    bag(format!("Bag{id}(tag=\"t\", items=[1])"), format!("Bag{id}(tag=\"t\", items=[1])")),
                    bag(format!("Bag{id}()"), format!("Bag{id}(tag=\"b\", items=[])")),
                    bag(
                        format!("Bag{id}(tag=\"\u{e9}\", items=[2, 3])"),
                        format!("Bag{id}(tag=\"\u{e9}\", items=[2, 3])"),
                    ),
                    bag(format!("Bag{id}(tag=\"none\")"), format!("Bag{id}(tag=\"none\", items=[])")),
                ]
            }
        }
    }
}

/// Every `PopTy`, for the generator to pick from.
pub(crate) const POP_TYS: [PopTy; 5] = [PopTy::Short, PopTy::Long, PopTy::Ints, PopTy::Point, PopTy::Bag];

/// A step of a `Popped` statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PopWay {
    /// `items.append(value)`.
    Pushed,
    /// `items.append(None)`.
    PushedNone,
    /// `got = items.pop() else missing`, printed.
    Kept,
    /// `stdout.print(items.pop() else missing)`.
    Inline,
    /// `_ = items.pop()`.
    Dropped,
    /// `items.pop(0) else missing`, printed, where there's an item.
    First,
    /// `items.pop(len(items) // 2) else missing`, printed, where there's an item.
    Middle,
    /// `items.pop(-1) else missing`, printed, where there's an item.
    Last,
    /// `items.remove(value) else missing`, printed.
    Removed,
    /// `items.discard(value)`.
    Discarded,
    /// `items.discard(None)`.
    DiscardedNone,
    /// `taken(ref items) else missing`, printed: popped through a `ref` parameter.
    ByRef,
    /// `pop() else missing` on a list made empty there, printed.
    Empty,
}

/// Every `PopWay`, for the generator to pick from.
pub(crate) const POP_WAYS: [PopWay; 13] = [
    PopWay::Pushed,
    PopWay::PushedNone,
    PopWay::Kept,
    PopWay::Inline,
    PopWay::Dropped,
    PopWay::First,
    PopWay::Middle,
    PopWay::Last,
    PopWay::Removed,
    PopWay::Discarded,
    PopWay::DiscardedNone,
    PopWay::ByRef,
    PopWay::Empty,
];

/// A `Sized` statement: `sized{id}(stdout)`, a number of one of the eight integer sizes changed a
/// step at a time and printed after each: checked arithmetic, `wrapping_` and `saturating_`
/// methods, shifts, bits, `sum`, widening, `parse` and a loop adding one a turn. What a step adds
/// or takes is worked out, as the program is written, from what the number holds then, so it
/// overflows, shifts past the width or divides by zero only where the step is meant to, which
/// stops the program.
#[derive(Clone, Debug)]
pub(crate) struct Sizing {
    pub(crate) id: usize,
    pub(crate) num: SizeNum,
    /// What the number starts at.
    pub(crate) start: i128,
    pub(crate) steps: Vec<SizedStep>,
}

/// One of the eight integer sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SizeNum {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl SizeNum {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            SizeNum::I8 => "i8",
            SizeNum::I16 => "i16",
            SizeNum::I32 => "i32",
            SizeNum::I64 => "i64",
            SizeNum::U8 => "u8",
            SizeNum::U16 => "u16",
            SizeNum::U32 => "u32",
            SizeNum::U64 => "u64",
        }
    }

    pub(crate) const fn bits(self) -> u32 {
        match self {
            SizeNum::I8 | SizeNum::U8 => 8,
            SizeNum::I16 | SizeNum::U16 => 16,
            SizeNum::I32 | SizeNum::U32 => 32,
            SizeNum::I64 | SizeNum::U64 => 64,
        }
    }

    pub(crate) const fn unsigned(self) -> bool {
        matches!(self, SizeNum::U8 | SizeNum::U16 | SizeNum::U32 | SizeNum::U64)
    }

    /// The least and the most it holds.
    pub(crate) const fn range(self) -> (i128, i128) {
        let bits = self.bits();
        if self.unsigned() {
            (0, (1_i128 << bits) - 1)
        } else {
            (-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1)
        }
    }

    /// How a value of it is written from an int: `int(...)` for `i64`, which is `int`, since
    /// `i64(...)` isn't a conversion yet (#2307).
    pub(crate) fn convert(self) -> &'static str {
        if self == SizeNum::I64 { "int" } else { self.name() }
    }

    /// What a value that's the type's least or most can be written as: an int literal, but past
    /// the largest int, which a `u64` literal can't be yet.
    pub(crate) fn literal(self, value: i128) -> String {
        if let Ok(fits) = i64::try_from(value) {
            format!("{}({fits})", self.convert())
        } else {
            // Past the largest int, only a `u64`, built from three that each fit.
            let third = value / 3;
            format!("(u64({third}) + u64({third}) + u64({}))", value - 2 * third)
        }
    }
}

/// Every `SizeNum`, for the generator to pick from.
pub(crate) const SIZE_NUMS: [SizeNum; 8] = [
    SizeNum::I8,
    SizeNum::I16,
    SizeNum::I32,
    SizeNum::I64,
    SizeNum::U8,
    SizeNum::U16,
    SizeNum::U32,
    SizeNum::U64,
];

/// A step of a `Sized` statement, with the value it uses, worked out as it's written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SizedStep {
    pub(crate) way: SizedWay,
    pub(crate) operand: i128,
}

/// How a `Sized` step changes or reads the number `x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SizedWay {
    /// `x += T(k)`.
    Added,
    /// `x -= T(k)`.
    Taken,
    /// `x *= T(k)`.
    Times,
    /// `x //= T(k)`.
    Halved,
    /// `x %= T(k)`.
    Remainder,
    /// `x = x.wrapping_add(T(k))`.
    WrappingAdd,
    /// `x = x.wrapping_sub(T(k))`.
    WrappingSub,
    /// `x = x.wrapping_mul(T(k))`.
    WrappingMul,
    /// `x = x.saturating_add(T(k))`.
    SaturatingAdd,
    /// `x = x.saturating_sub(T(k))`.
    SaturatingSub,
    /// `x = x.saturating_mul(T(k))`.
    SaturatingMul,
    /// `x = x << T(k)`: the bits past the top dropped.
    Shifted,
    /// `x = x >> T(k)`.
    ShiftedDown,
    /// `x = x & T(k)`.
    Masked,
    /// `x = x | T(k)`.
    Ored,
    /// `x = x ^ T(k)`.
    Xored,
    /// `x = ~x`.
    Inverted,
    /// `int(x) * 3` and `float(x) / 2`, printed.
    Widened,
    /// `sum([x, T(k)])`, printed.
    Summed,
    /// `for _ in range(k): x += T(1)`: a bound the proofs can't see past.
    Looped,
    /// `T.parse("k") except why:`, printed, or refused past what it holds.
    Parsed,
}

/// Every `SizedWay`, for the generator to pick from.
pub(crate) const SIZED_WAYS: [SizedWay; 21] = [
    SizedWay::Added,
    SizedWay::Taken,
    SizedWay::Times,
    SizedWay::Halved,
    SizedWay::Remainder,
    SizedWay::WrappingAdd,
    SizedWay::WrappingSub,
    SizedWay::WrappingMul,
    SizedWay::SaturatingAdd,
    SizedWay::SaturatingSub,
    SizedWay::SaturatingMul,
    SizedWay::Shifted,
    SizedWay::ShiftedDown,
    SizedWay::Masked,
    SizedWay::Ored,
    SizedWay::Xored,
    SizedWay::Inverted,
    SizedWay::Widened,
    SizedWay::Summed,
    SizedWay::Looped,
    SizedWay::Parsed,
];

/// What a `Sized` step uses, worked out from what `x` holds as it's written: one that keeps the
/// step from stopping the program, or, where it `breaks` and can, one that stops it. `pick`, from
/// 0 to 3, chooses among the values that do.
pub(crate) fn sized_operand(num: SizeNum, x: i128, way: SizedWay, breaks: bool, pick: usize) -> i128 {
    let (least, most) = num.range();
    let bits = i128::from(num.bits());
    let choose = |from: [i128; 4]| from.get(pick).copied().unwrap_or(1);
    let small = choose([1, 3, 9, 100]);
    match way {
        SizedWay::Added | SizedWay::Summed if breaks && x >= 1 => most,
        SizedWay::Added | SizedWay::Summed => small.min((most - x).max(0)),
        SizedWay::Taken if breaks && num.unsigned() && x < most => x + 1,
        SizedWay::Taken if breaks && x <= -2 => most,
        SizedWay::Taken => small.min((x - least).max(0)),
        SizedWay::Times if breaks && (x >= 2 || x <= -2) => most,
        SizedWay::Times => {
            [choose([2, 3, 0, 1]), 1].into_iter().find(|k| (least..=most).contains(&(x * k))).unwrap_or(1)
        }
        SizedWay::Halved | SizedWay::Remainder if breaks => 0,
        SizedWay::Halved | SizedWay::Remainder => choose([2, 3, 7, 1]),
        SizedWay::WrappingAdd
        | SizedWay::WrappingSub
        | SizedWay::WrappingMul
        | SizedWay::SaturatingAdd
        | SizedWay::SaturatingSub
        | SizedWay::SaturatingMul => choose([1, 7, most / 2, most]),
        SizedWay::Shifted | SizedWay::ShiftedDown if breaks => bits,
        SizedWay::Shifted | SizedWay::ShiftedDown => choose([1, 3, bits - 1, 0]),
        SizedWay::Masked | SizedWay::Ored | SizedWay::Xored => choose([15, 85, most / 3, 0]),
        SizedWay::Inverted | SizedWay::Widened => 0,
        SizedWay::Looped if breaks && most - x < 64 => most - x + 1,
        SizedWay::Looped => choose([1, 3, 5, 2]).min((most - x).max(0)),
        // Past what it holds is refused, which goes on: an int's text past the largest isn't
        // read as an int at all, so `i64` keeps to what it holds.
        SizedWay::Parsed if breaks && num != SizeNum::I64 => most + 1,
        SizedWay::Parsed => choose([least, most, 7, 0]),
    }
}

/// A `Sized` step for `x` as it is: `way` with what [`sized_operand`] works out for it, unless
/// that would stop the program when it's not meant to, as widening a `u64` past the largest int
/// would, where it's `~x` instead, which never does.
pub(crate) fn sized_choice(num: SizeNum, x: i128, way: SizedWay, breaks: bool, pick: usize) -> SizedStep {
    let operand = sized_operand(num, x, way, breaks, pick);
    if !breaks && sized_step(num, x, way, operand).is_err() {
        return SizedStep { way: SizedWay::Inverted, operand: 0 };
    }
    SizedStep { way, operand }
}

/// What a `Sized` step does to `x`, as compiled code does it: the number after it, or what stops
/// the program, in compiled code's words.
pub(crate) fn sized_step(num: SizeNum, x: i128, way: SizedWay, k: i128) -> Result<i128, String> {
    let (least, most) = num.range();
    let article = if num.unsigned() { "a" } else { "an" };
    let overflows = || {
        if num == SizeNum::I64 {
            "this calculation overflows an int".to_owned()
        } else {
            format!("this calculation overflows {article} {}, which holds {least} to {most}", num.name())
        }
    };
    let checked = |value: i128| if (least..=most).contains(&value) { Ok(value) } else { Err(overflows()) };
    let width = 1_i128 << num.bits();
    let wrapped = |value: i128| {
        let low = value.rem_euclid(width);
        if low > most { low - width } else { low }
    };
    let floor = |a: i128, b: i128| {
        let (quotient, remainder) = (a / b, a % b);
        if remainder != 0 && (remainder < 0) != (b < 0) { quotient - 1 } else { quotient }
    };
    match way {
        SizedWay::Added | SizedWay::Summed => checked(x + k),
        SizedWay::Taken => checked(x - k),
        SizedWay::Times => x.checked_mul(k).map_or_else(|| Err(overflows()), checked),
        SizedWay::Halved | SizedWay::Remainder if k == 0 => Err("division by zero".to_owned()),
        SizedWay::Halved => checked(floor(x, k)),
        SizedWay::Remainder => Ok(x - k * floor(x, k)),
        SizedWay::WrappingAdd => Ok(wrapped(x + k)),
        SizedWay::WrappingSub => Ok(wrapped(x - k)),
        // The low bits of the product, as the bits each holds multiply.
        SizedWay::WrappingMul => {
            let bits_of = |value: i128| value.rem_euclid(width).cast_unsigned();
            Ok(wrapped(bits_of(x).wrapping_mul(bits_of(k)).rem_euclid(width.cast_unsigned()).cast_signed()))
        }
        SizedWay::SaturatingAdd => Ok((x + k).clamp(least, most)),
        SizedWay::SaturatingSub => Ok((x - k).clamp(least, most)),
        SizedWay::SaturatingMul => {
            Ok(x.checked_mul(k).unwrap_or(if (x < 0) == (k < 0) { most } else { least }).clamp(least, most))
        }
        SizedWay::Shifted | SizedWay::ShiftedDown if !(0..i128::from(num.bits())).contains(&k) => {
            if num.bits() == 64 {
                Err("can't shift by a count outside 0 to 63".to_owned())
            } else {
                Err(format!("can't shift {} by a count outside 0 to {}", num.name(), num.bits() - 1))
            }
        }
        SizedWay::Shifted => Ok(wrapped(x << k)),
        SizedWay::ShiftedDown => Ok(x >> k),
        SizedWay::Masked => Ok(x & k),
        SizedWay::Ored => Ok(wrapped(x | k)),
        SizedWay::Xored => Ok(wrapped(x ^ k)),
        SizedWay::Inverted if num.unsigned() => Ok(most - x),
        SizedWay::Inverted => Ok(!x),
        SizedWay::Widened if x > i128::from(i64::MAX) => {
            Err(format!("it doesn't fit in int, which holds {} to {}: {x}", i64::MIN, i64::MAX))
        }
        SizedWay::Looped => {
            if x + k > most {
                Err(overflows())
            } else {
                Ok(x + k)
            }
        }
        SizedWay::Widened | SizedWay::Parsed => Ok(x),
    }
}

/// An `Arrayed` statement: `arrayed{id}(stdout)`, a `Grid{id}` of fixed-size arrays, of ints, of
/// structs holding a list, of optional structs, of texts and of arrays, held in a list, a Table and
/// a dict too, beside a loose array, then a step at a time, then what each holds. An array is held
/// inline, so a plain `=` copies it and each copy changes apart, and an index past either end stops
/// the program however it's reached.
#[derive(Clone, Debug)]
pub(crate) struct Arraying {
    pub(crate) id: usize,
    pub(crate) steps: Vec<ArrayStep>,
}

/// A step of an `Arrayed` statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArrayStep {
    pub(crate) way: ArrayWay,
    /// The place it picks, from 0 to 3, taken modulo the length of the array it indexes.
    pub(crate) at: usize,
    /// What it writes or adds, from -9 to 9.
    pub(crate) value: i64,
    /// Whether a `Looped` or `Picked` index goes past the array's end, which stops the program.
    pub(crate) past: bool,
}

impl ArrayStep {
    /// The bound a `Looped` step's loop reaches: from 1 to 4, or 5 past the end of `counts`.
    pub(crate) fn bound(self) -> i64 {
        if self.past { 5 } else { 1 + i64::try_from(self.at % 4).unwrap_or(0) }
    }

    /// The index a `Picked` step writes at: from -4 to 3, counting from the end when it's
    /// negative, or 4 or -5, one past either end of `counts`.
    pub(crate) fn picked(self) -> i64 {
        let at = i64::try_from(self.at % 4).unwrap_or(0);
        match (self.past, at % 2 == 0) {
            (true, true) => 4,
            (true, false) => -5,
            (false, _) if self.value < 0 => at - 4,
            (false, _) => at,
        }
    }
}

/// How an `Arrayed` step goes. `counts` is an `array[int, 4]`, `cells` an `array[Cell{id}, 3]` of
/// structs holding a list, `slots` an `array[Cell{id}?, 2]`, `names` an `array[str, 2]` and `rows`
/// an `array[array[int, 2], 2]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArrayWay {
    /// `grid.counts[at] = value`.
    Set,
    /// `grid.counts[at] += value`.
    Added,
    /// `grid.counts[-1 - at] += value`: counted from the end.
    FromEnd,
    /// `copy = grid.counts`, `copy[at] = value`, both printed: the copy changes apart.
    Copied,
    /// `grid.counts = loose`, then `loose[at] = value`: the grid keeps what it was given.
    Replaced,
    /// `for ref each in grid.counts: each += value`.
    ForRef,
    /// `for ref cell in grid.cells`, each one's `hp` added to and a tag appended.
    ForRefCells,
    /// `grid.cells[at].hp = value`.
    CellSet,
    /// `grid.cells[at].tags.append(...)`: a list in a struct in an array.
    CellTagged,
    /// `grid.slots[at] = Cell{id}(hp=value)`.
    SlotFilled,
    /// `grid.slots[at] = None`.
    SlotCleared,
    /// `for ref slot in grid.slots`, each one there added to.
    ForRefSlots,
    /// `grid.slots[at] is None`, printed, and the `hp` of what's there or else of `Cell{id}()`.
    SlotRead,
    /// `grids[at].counts[at] += value`: an array in a struct in a list.
    InList,
    /// `for ref each in grids: each.counts[at] += value`.
    ForRefList,
    /// `table[first].counts[at] = value`: an array in a struct in a Table.
    InTable,
    /// `by_name["k"].counts[at] += value`: an array in a struct in a dict.
    InDict,
    /// `bumped{id}(ref grid.counts, by=value)`: an array given by `ref`.
    ByRef,
    /// `summed{id}(grid.counts)`, printed: an array given as a value and copied inside, the copy
    /// changed, which the caller's must not see.
    ByValue,
    /// `value in grid.counts`, `len` of each array, `grid.counts == other.counts` and `list(...)`,
    /// printed.
    Read,
    /// `grid.rows[at][at + 1] += value`: an array of arrays.
    Rows,
    /// `row = grid.rows[at]`, `row[1] = value`, both printed.
    RowCopied,
    /// `grid.names[at] += ...`: a text in an array.
    Named,
    /// `whole = grid`, its counts and its first cell's tags changed, both printed: a struct's
    /// arrays copied with it, lists inside them too.
    WholeCopied,
    /// `for k in range(at{id}(n)): grid.counts[k] += value`, with `n` from 1 to 4, or 5 when it's
    /// `past`: a bound the proofs can't see, so each index keeps its check.
    Looped,
    /// `grid.counts[at{id}(i)] = value`, with `i` from -4 to 3, or 4 or -5 when it's `past`.
    Picked,
}

/// Every `ArrayWay`, for the generator to pick from.
pub(crate) const ARRAY_WAYS: [ArrayWay; 26] = [
    ArrayWay::Set,
    ArrayWay::Added,
    ArrayWay::FromEnd,
    ArrayWay::Copied,
    ArrayWay::Replaced,
    ArrayWay::ForRef,
    ArrayWay::ForRefCells,
    ArrayWay::CellSet,
    ArrayWay::CellTagged,
    ArrayWay::SlotFilled,
    ArrayWay::SlotCleared,
    ArrayWay::ForRefSlots,
    ArrayWay::SlotRead,
    ArrayWay::InList,
    ArrayWay::ForRefList,
    ArrayWay::InTable,
    ArrayWay::InDict,
    ArrayWay::ByRef,
    ArrayWay::ByValue,
    ArrayWay::Read,
    ArrayWay::Rows,
    ArrayWay::RowCopied,
    ArrayWay::Named,
    ArrayWay::WholeCopied,
    ArrayWay::Looped,
    ArrayWay::Picked,
];

/// A `Generated` statement: `generated{id}(stdout)`, a step at a time, each a function looping
/// over generators that fail partway, `failing{id}(limit, fail_at=...)`, which yields from 0 and
/// fails with `Shut{id}(code=fail_at)` there, if it's below `limit`. Each step's failure is caught
/// and printed by its caller. The steps wrap a failure with `? from` in a `for`, in `enumerate` and
/// in `zip` on either side or both, and hold failing generators in a name, a generator kept for
/// later, a tuple, a list and a generic struct, each looped over where it's made.
#[derive(Clone, Debug)]
pub(crate) struct Generating {
    pub(crate) id: usize,
    pub(crate) steps: Vec<GenStep>,
}

/// A step of a `Generated` statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GenStep {
    pub(crate) way: GenWay,
    /// How many items the first generator yields, from 1 to 5, or how many generators a `KeptOver`
    /// makes.
    pub(crate) limit: i64,
    /// Where the first generator fails, from 0 to 6: past `limit`, it doesn't.
    pub(crate) fail_at: i64,
    /// Where the second one fails, from 0 to 6, or where a `Broken` loop breaks. The second yields
    /// one more than the first.
    pub(crate) other_at: i64,
}

/// How a `Generated` step loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenWay {
    /// `for value in failing(...)? from Lost{id}(step=n)`.
    ForFrom,
    /// `enumerate(failing(...)? from Lost{id}(step=n))`.
    EnumerateFrom,
    /// `zip(failing(...)? from Left{id}(...), failing(...)? from Right{id}(...))`: the failure of
    /// the side that failed, wrapped as it says, the first side asked first.
    ZipBothFrom,
    /// `zip(failing(...)? from Left{id}(...), failing(...)?)`: the second's failure passes up as
    /// it is.
    ZipLeftFrom,
    /// `zip(failing(...)?, failing(...)? from Right{id}(...))`.
    ZipRightFrom,
    /// `zip([10, 20, 30], failing(...)? from Right{id}(...))`.
    ZipListFrom,
    /// `for value in failing(...)? from Lost{id}(...)`, breaking at `other_at`.
    Broken,
    /// `held = failing(...)?`, a line printed, then `for value in held`.
    Named,
    /// `(failing(size + 1, ...)? for size in range(limit))`, a generator kept for later whose items
    /// are failing generators, each looped over in turn.
    KeptOver,
    /// `first, second = (failing(...)?, failing(...)?)`, each looped over.
    InTuple,
    /// `pair = (failing(...)?, n)`, `for value in pair[0]`.
    TupleIndexed,
    /// `[failing(...)?, failing(...)?]`, each looped over.
    InList,
    /// `Boxed{id}(item=failing(...)?)`: a generic struct holding one, looped over through its
    /// field.
    InBox,
}

/// Every `GenWay`, for the generator to pick from.
pub(crate) const GEN_WAYS: [GenWay; 13] = [
    GenWay::ForFrom,
    GenWay::EnumerateFrom,
    GenWay::ZipBothFrom,
    GenWay::ZipLeftFrom,
    GenWay::ZipRightFrom,
    GenWay::ZipListFrom,
    GenWay::Broken,
    GenWay::Named,
    GenWay::KeptOver,
    GenWay::InTuple,
    GenWay::TupleIndexed,
    GenWay::InList,
    GenWay::InBox,
];

/// How a `Turned` loop's int changes sign after its first turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Turn {
    /// `at = start`, then `at -= step` each turn: stepped down past 0.
    StepDown,
    /// `sign * start`, with `sign` flipped each turn.
    Flip,
    /// `n = start`, then `n += 1` each turn, reset to `reset_to`, below 0, on turn `reset_at`.
    Reset,
}

/// The list a `Turned` loop indexes: 4 items, so the places from -4 to 3 are in it.
pub(crate) const XS: [i64; 4] = [10, 20, 30, 40];

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
    /// As [`Self::QuestionInside`], with a `?` nested inside the call's argument too, and the
    /// inner call the one whose limit is `at - 1`: `capped(capped(i, limit=at - 1)?, limit=1000)?`.
    QuestionNestedInner,
    /// As [`Self::QuestionNestedInner`], with the outer call the one whose limit is `at - 1`.
    QuestionNestedOuter,
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
    /// `got = capped(i, limit=at - 1) except why:`, printing `why` and returning: handled, so
    /// the cleanups after `lend` run.
    ExceptReturn,
    /// The same, with `continue`, which every round from `at` on takes.
    ExceptContinue,
    /// `got = capped(i, limit=at - 1) except Refused(): -1`, one case on the line, giving a value
    /// and carrying on.
    ExceptValue,
    /// `except why:` with one arm, `case _:`, that prints `why` and breaks.
    ExceptArms,
}

impl Leave {
    /// Whether leaving this way is a failure, which runs the cleanups in `lend x except:`, rather
    /// than the code after `lend`.
    pub(crate) const fn fails(self) -> bool {
        matches!(
            self,
            Self::ElseFail
                | Self::Question
                | Self::QuestionInside
                | Self::QuestionNestedInner
                | Self::QuestionNestedOuter
        )
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
    /// `through_shown(way, limit, stop, words)`: a loop over `enumerate` or `zip` of a generator
    /// made right there (`ZipWay`). The generator lends from a source, summed until an item is
    /// `stop` and let go there or when the list runs out, or fails partway, at `stop`.
    ZipSummed(ZipSumming),
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
    /// `turned{id}(stdout)`: a loop of `rounds` turns whose int is at least 0 on the first turn
    /// and changes sign later, by `how`. Each turn prints it, `XS[it]`, it times 3, and it added
    /// to each end of `int`, so a proof that it's never negative, made from the first turn alone,
    /// shows: from -4 to 3 all of those hold, and past them the program stops.
    Turned(Turning),
    /// `ordered{id}(stdout)`: lines whose parts change what's read before them, read left to right.
    Ordered(InOrder),
    /// `locked_together{id}(stdout)`: two `shared` values taken in one `lock`, changed and put back.
    LockedTogether(LockingTogether),
    /// `arrayed{id}(stdout)`: fixed-size arrays held inline, copied by `=` and changed apart.
    Arrayed(Arraying),
    /// `popped{id}(stdout)`: optionals of a heap type put in a list and taken out every way.
    Popped(Popping),
    /// `sized{id}(stdout)`: a number of one integer size, changed every way it can be.
    Sized(Sizing),
    /// `generated{id}(stdout)`: generators that fail partway, wrapped with `? from` and held in
    /// names, tuples, lists, generic structs and a generator kept for later.
    Generated(Generating),
    /// `ruled{id}(stdout)`: fields with `where` rules, written every way, lent by `ref` and copied.
    Ruled(Ruling),
    /// `saved{id}(stdout)`: a box of the program's own values saved and loaded back through JSON,
    /// `std.pack` and TOML, and texts that don't hold one refused.
    Saved(super::saving::Saving),
    /// `stored{id}(fs, stdout)`: a SQLite database's one table, its rows put in, changed, taken
    /// out and read back, some breaking its rules, and transactions kept or undone.
    Stored(super::storing::Storing),
    /// `crewed{id}(stdout)`: a `shared Bag` changed under its lock by tasks one after another, each
    /// taking every number it's sent through two or three locks, which may crash partway, and read
    /// by `main` under its lock after each.
    Crewed(super::crews::Crew),
    /// `pooled{id}(stdout)`: a `std.concurrent` pool of links, taken by `main` nested up to its
    /// size, some blocks failing, a task crashing with one, and a short wait timing out.
    Pooled(super::pools::Pooling),
    /// `messaged{id}(stdout)`: enums of one shape, flat or not, sent to a task that prints what each
    /// held, while the sender changes its own text.
    Messaged(super::messaging::Messaging),
    /// `opened{id}(stdout)`: one `with` opening two or three handles left to right, one perhaps
    /// failing to open, a lent pair taken apart inside, and the block ending, failing or crashing,
    /// with cleanups that may crash.
    Opened(super::opening::Opening),
    /// `interfaced{id}(stdout)`: two types with one interface, through generic functions bounded by it.
    Interfaced(Interfacing),
    /// `generic{id}(stdout)`: values of one type through a generic holder and generic functions.
    Generic(Genericking),
    /// `stuck{id}(stdout)`: a wait nothing ends, timing out or stopping the program as a deadlock.
    Stuck(Sticking),
    /// `filed{id}(fs, stdout)`: files written whole or left as they were, a step at a time.
    Filed(Filing),
    /// `owned{id}(stdout)`: values of types with their own `def []`, changed through it.
    Owned(OwnIndexing),
    /// `streamed{id}(stdout)`: a generator over `parallel(xs)`, read in one of the ways a stream is.
    Streamed(Streaming),
    /// `aliased{id}(stdout)`: copies of collections and their parts, each changed in place, then
    /// all printed.
    Aliased(Aliasing),
    /// `constants{id}(stdout)`: module constants read on the main thread, then used and changed
    /// in copies there, in a launched task and in `parallel`'s bodies.
    Constants(Constants),
    /// `fielded{id}(stdout)`: `+=` on a struct's fields with calls on the right.
    FieldCalls(FieldCalls),
    /// `yielded{id}(stdout)`: generators yielding structs, tuples and enums holding texts, lists
    /// and dicts, and an inbox loop sending them back.
    Yielded(Yielding),
    /// `woken{id}(stdout)`: a task that crashes or fails while its launcher waits on a channel.
    Woken(Waking),
    /// `swaps{id}(stdout)`: a lookup kept while its struct is set to another, then a store.
    Swapped(Swapping),
    /// `shelved{id}(stdout)`: crashes inside a `ref self` method on part of a lock's value.
    Shelved(Shelving),
    /// `listened{id}(stdout)`: listeners of a channel to every listener, some falling behind.
    Listened(Listening),
    /// `ref_cased{id}(stdout)`: a `match` with `ref` written one of the ways the compiled program
    /// once put back wrong, or couldn't build.
    RefCased(RefCasing),
    /// `captured{id}(stdout)`: a name written where another of the same name and type is in
    /// sight, in a way the checker once renamed into the other.
    Captured(Capturing),
    /// `narrowed{id}(stdout)`: a union narrowed to one member and changed through it, by a `ref self`
    /// method or a `ref` call.
    Narrowed(Narrowing),
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
    /// `for name in range(len(list)):` reading `list[name]` first each round, with the body free
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
    /// `if len(nodes) > 0: nodes[at % len(nodes)] = node`.
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
    /// place, and both printed, under `if len(holder.items) < 64:`.
    GrownBy(String, String),
    /// `if len(list) < 64:` then `own = list`, `appended_all(ref list, own)` and both printed:
    /// a list lent to a call through `ref` beside a value read from it.
    LentRef(String, String),
    /// `mut rows = [items, [7]]`, then under `if len(rows[0]) < 64:`, `own = rows[0]`,
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

/// Where a generator made right there sits in a loop over `enumerate` or `zip`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZipWay {
    Enumerated,
    First,
    Second,
}

/// Every place a generator sits in `enumerate` or `zip`, for a test to write each.
pub(crate) const ZIP_WAYS: [ZipWay; 3] = [ZipWay::Enumerated, ZipWay::First, ZipWay::Second];

/// A loop over `enumerate` or `zip` of a generator made right there: the one lending from a
/// source (`lent_counts`) or one failing partway (`failing_counts`), with a list of `words` items,
/// `1` to `words`, beside it in `zip`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ZipSumming {
    pub(crate) way: ZipWay,
    pub(crate) failing: bool,
    pub(crate) limit: i64,
    pub(crate) stop: i64,
    pub(crate) words: i64,
}

impl ZipSumming {
    /// The number `through_shown` takes for this way.
    pub(crate) fn number(&self) -> i64 {
        let at = match self.way {
            ZipWay::Enumerated => 0,
            ZipWay::First => 1,
            ZipWay::Second => 2,
        };
        if self.failing { at + 3 } else { at }
    }
}
